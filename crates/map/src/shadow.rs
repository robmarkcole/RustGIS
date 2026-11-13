//! Cast shadows and sky visibility for Shaded Relief (opengeos/RustGIS#1782): how much of the sun
//! each cell of an elevation raster sees over the terrain between it and the sun ([`light`]), and
//! how much of the sky it sees over the terrain around it ([`sky`]).
//!
//! The sun is a disc [`SOFT_EDGE`] degrees either side of its altitude, so a shadow has a soft
//! edge. Each cell traces the terrain toward the sun's azimuth one column or row at a time,
//! reading heights between cells (bilinear: reading the nearest cell instead over-shadows diagonal
//! suns, franciscoparrao/surtgis#153), and keeps the steepest angle up to the terrain it passes.
//! The share of the disc above that angle is the light the cell keeps. The trace stops once
//! nothing farther can change it: the terrain left can't rise above the angle kept (or the disc's
//! lower edge), or the disc is already hidden. The sky is traced likewise, in [`SKY_DIRECTIONS`]
//! directions, reading the two cells either side of each step. Cells outside the raster cast no shadow on it, and hide none of
//! its sky.

use std::sync::atomic::{AtomicBool, Ordering};

use rustgis_data::raster::Raster;

use crate::raster::RasterMask;

/// Half the sun's apparent size, in degrees: how wide a shadow's soft edge is. Far larger than the
/// real sun's quarter of a degree, for a soft edge at map scale.
pub const SOFT_EDGE: f64 = 5.0;

/// The most cells a raster's shadows, or its sky, are worked out for in the browser, which has one
/// thread: past it, the relief is drawn without them, and said.
pub const MAX_BROWSER_CELLS: usize = 500_000;

/// Cells below which the work stays on one thread: starting threads costs more.
const ONE_THREAD_BELOW: usize = 64 * 1024;

/// Why there is no light or sky grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unlit {
    /// `stop` was set: a newer sun or raster replaced it.
    Stopped,
    /// The grid couldn't be held.
    NoMemory,
    /// A thread working it out failed (panicked).
    Failed,
}

/// The light left by cast shadows on each cell of `band` (`0` in shadow to `255` lit), row by row
/// as the raster's pixels are, for a sun at `azimuth` (degrees clockwise from north) and `altitude`
/// (degrees above the horizon), heights scaled by `z_factor`. Cells that are no data or masked are
/// lit and cast no shadow. Rows are split over up to `threads` threads, each the same as on one.
/// Looks at `stop` between rows, and at the run's Cancel (see [`rustgis_data::progress`]) on a
/// thread that has one (the browser, which works it out where it is asked). On the desktop the
/// worker has none: a cancelled export that waited for it alone sets `stop`. A sun under about 5°
/// traces far over gentle ground: the cost grows with the raster's width as well as its cells.
pub fn light(
    r: &Raster,
    band: usize,
    mask: &RasterMask,
    azimuth: f64,
    altitude: f64,
    z_factor: f64,
    threads: usize,
    stop: &AtomicBool,
) -> Result<Vec<u8>, Unlit> {
    let sun = Sun::new(r, azimuth, altitude);
    each_row(r, band, mask, z_factor, threads, stop, |trace, row, out| {
        for (col, cell) in out.iter_mut().enumerate() {
            if let Some(z0) = trace.at(col, row) {
                *cell = sun.lit(trace.horizon(&sun.way, (col, row, z0), sun.low, sun.high));
            }
        }
    })
}

/// The directions, evenly spread from north, a cell's horizon is looked for in, for its sky.
pub const SKY_DIRECTIONS: usize = 16;

/// How far a cell's horizon is looked for, in cells (the longer side of one): terrain farther
/// hides none of its sky.
pub const SKY_REACH: f64 = 100.0;

/// The sky's trace takes one step (a row or a column) at a time up to 31, then the steps taken
/// over this many (2 at 32, 6 at 96): half the reads, the horizon found to within a sixteenth of
/// its distance, and a peak narrower than that far away can be missed.
pub const SKY_SPREAD: usize = 16;

/// The share of the sky each cell of `band` sees (`0` none to `255` all of it, as open flat ground
/// does), row by row as the raster's pixels are, heights scaled by `z_factor`: the share of the
/// half sphere above its horizon, `1 − mean(sin γ)` over [`SKY_DIRECTIONS`] directions, `γ` being
/// the angle up to the horizon in each (0 where the terrain falls away, so a peak sees the whole
/// sky), looked for up to [`SKY_REACH`] cells away, in steps [`SKY_SPREAD`] spreads. Heights are
/// read from the two cells either side of each step, the nearer one where the other is no data;
/// cells that are no data or masked see the whole sky and hide none of it. Threads, `stop` and
/// Cancel are as [`light`] has them.
///
/// This is the sky-view factor of Zakšek, Oštir and Kokalj, "Sky-View Factor as a Relief
/// Visualization Technique", Remote Sensing 3(2), 398–415, 2011 (doi:10.3390/rs3020398), its
/// horizon traced here; no other code was followed.
pub fn sky(r: &Raster, band: usize, mask: &RasterMask, z_factor: f64, threads: usize, stop: &AtomicBool) -> Result<Vec<u8>, Unlit> {
    let (cx, cy) = r.cell_size_m();
    let reach = SKY_REACH * cx.max(cy);
    let longest = r.width.max(r.height) as usize;
    let ways: Vec<Vec<Read>> =
        (0..SKY_DIRECTIONS).map(|i| reads(&Way::new(r, i as f64 * 360.0 / SKY_DIRECTIONS as f64), reach, longest)).collect();
    each_row(r, band, mask, z_factor, threads, stop, |trace, row, out| {
        for (col, cell) in out.iter_mut().enumerate() {
            let Some(z0) = trace.at(col, row) else { continue };
            let hidden: f64 = ways
                .iter()
                .map(|way| {
                    let t = trace.skyline(way, col, row, z0);
                    // sin(atan t)
                    t / (1.0 + t * t).sqrt()
                })
                .sum();
            *cell = ((1.0 - hidden / ways.len() as f64).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    })
}

/// One read of a cell's sky trace, the same for every cell: the cell before the point read, in
/// columns and rows from the cell traced; the next one's weight, along the row (`along_row`) or
/// down the column (each step of a trace is on one or the other); 1 / the distance in metres.
struct Read {
    dx: isize,
    dy: isize,
    weight: f64,
    along_row: bool,
    per_metre: f64,
}

/// The reads of a sky trace along `way`, up to `reach` metres and at most `steps` steps (no trace
/// goes past the raster's longer side, whatever its cells' size says): one step at a time, then
/// steps of the steps taken over [`SKY_SPREAD`].
fn reads(way: &Way, reach: f64, steps: usize) -> Vec<Read> {
    let mut out = Vec::new();
    let mut n = 1usize;
    while n <= steps && n as f64 * way.metres <= reach {
        let k = n as f64;
        let (x, y) = (way.step.0 * k, -way.step.1 * k);
        let (fx, fy) = (x - x.floor(), y - y.floor());
        out.push(Read {
            dx: x.floor() as isize,
            dy: y.floor() as isize,
            weight: if fy == 0.0 { fx } else { fy },
            along_row: fy == 0.0,
            per_metre: 1.0 / (k * way.metres),
        });
        n += (n / SKY_SPREAD).max(1);
    }
    out
}

/// A grid of `255` the size of `r`, each row filled by `row` (the trace, the row, its cells),
/// rows split over up to `threads` threads; filled no further when the band isn't there or every
/// cell is no data or masked. Looks at `stop` and the run's Cancel between rows (see [`light`]).
fn each_row(
    r: &Raster,
    band: usize,
    mask: &RasterMask,
    z_factor: f64,
    threads: usize,
    stop: &AtomicBool,
    row: impl Fn(&Trace, usize, &mut [u8]) + Sync,
) -> Result<Vec<u8>, Unlit> {
    let (w, h) = (r.width as usize, r.height as usize);
    let cells = w.checked_mul(h).ok_or(Unlit::NoMemory)?;
    let mut out = Vec::new();
    out.try_reserve_exact(cells).map_err(|_| Unlit::NoMemory)?;
    out.resize(cells, 255u8);
    let Some(data) = r.bands.get(band).and_then(|b| b.data.get(..cells)) else {
        return Ok(out);
    };
    // The heights read, NaN where the cell is no data or masked: checked once per cell here, not
    // at each of the many reads a cell's trace makes (4 bytes a cell while it is worked out).
    let mut heights = Vec::new();
    heights.try_reserve_exact(cells).map_err(|_| Unlit::NoMemory)?;
    heights.extend(data.iter().map(|v| if r.is_valid(*v) && !mask.hides(*v) { *v } else { f32::NAN }));
    let mut trace = Trace { heights: &heights, w, h, top: f64::NEG_INFINITY, z_factor };
    trace.top = (0..cells).filter_map(|i| trace.height(i)).fold(f64::NEG_INFINITY, f64::max);
    if !trace.top.is_finite() || w == 0 {
        return Ok(out);
    }
    let trace = &trace;
    let threads = if cells < ONE_THREAD_BELOW { 1 } else { threads.clamp(1, h) };
    let rows = h.div_ceil(threads);
    let chunks: Vec<(usize, &mut [u8])> = out.chunks_mut(rows * w).enumerate().collect();
    let run = |(i, chunk): (usize, &mut [u8])| -> Result<(), Unlit> {
        for (dy, cells) in chunk.chunks_mut(w).enumerate() {
            if stop.load(Ordering::Relaxed) || rustgis_data::progress::check().is_err() {
                return Err(Unlit::Stopped);
            }
            row(trace, i * rows + dy, cells);
        }
        Ok(())
    };
    if threads <= 1 {
        chunks.into_iter().try_for_each(run)?;
    } else {
        let run = &run;
        // Each part in a slot its thread takes it from: one the system refuses to start leaves it
        // there, and it runs here (a refused thread costs speed, not the grid).
        let slots: Vec<std::sync::Mutex<Option<(usize, &mut [u8])>>> = chunks.into_iter().map(|c| std::sync::Mutex::new(Some(c))).collect();
        let results = std::thread::scope(|scope| {
            let mut refused = Vec::new();
            let mut handles = Vec::new();
            for slot in &slots {
                match std::thread::Builder::new().spawn_scoped(scope, move || take(slot).map_or(Ok(()), run)) {
                    Ok(handle) => handles.push(handle),
                    Err(_) => refused.push(slot),
                }
            }
            let mut results: Vec<Result<(), Unlit>> = refused.into_iter().filter_map(take).map(run).collect();
            results.extend(handles.into_iter().map(|h| h.join().unwrap_or(Err(Unlit::Failed))));
            results
        });
        results.into_iter().collect::<Result<(), Unlit>>()?;
    }
    Ok(out)
}

/// What a slot holds, taken out once.
fn take<T>(slot: &std::sync::Mutex<Option<T>>) -> Option<T> {
    slot.lock().ok().and_then(|mut c| c.take())
}

/// A direction a cell's trace takes.
struct Way {
    /// A step along it, in columns and rows: one column or one row, whichever is more.
    step: (f64, f64),
    /// That step's length on the ground, in metres.
    metres: f64,
}

impl Way {
    /// Toward `azimuth`, degrees clockwise from north.
    fn new(r: &Raster, azimuth: f64) -> Self {
        let (cx, cy) = r.cell_size_m();
        let (cx, cy) = (cx.max(1e-9), cy.max(1e-9));
        let (sin, cos) = azimuth.to_radians().sin_cos();
        // East is along the columns when the pixel width is positive; north is up the rows when
        // the pixel height is negative (north up), down them when it is positive (bottom up).
        let east = sin / cx * r.transform.pixel_width.signum();
        let north = cos / cy * -r.transform.pixel_height.signum();
        let longest = east.abs().max(north.abs()).max(1e-12);
        let step = (east / longest, north / longest);
        let metres = ((step.0 * cx).powi(2) + (step.1 * cy).powi(2)).sqrt();
        Way { step, metres }
    }
}

/// The sun as each cell's trace needs it.
struct Sun {
    way: Way,
    /// The tangents of the disc's lower and upper edges.
    low: f64,
    high: f64,
    altitude: f64,
}

impl Sun {
    fn new(r: &Raster, azimuth: f64, altitude: f64) -> Self {
        let altitude = altitude.clamp(0.0, 90.0);
        let edge = |a: f64| a.clamp(-89.9, 89.9).to_radians().tan();
        Sun { way: Way::new(r, azimuth), low: edge(altitude - SOFT_EDGE), high: edge(altitude + SOFT_EDGE), altitude }
    }

    /// The share of the disc above the steepest tangent `t` seen toward it, 0 to 255.
    fn lit(&self, t: f64) -> u8 {
        if t <= self.low {
            return 255;
        }
        if t >= self.high {
            return 0;
        }
        let seen = t.atan().to_degrees();
        let share = (self.altitude + SOFT_EDGE - seen) / (2.0 * SOFT_EDGE);
        (share.clamp(0.0, 1.0) * 255.0).round() as u8
    }
}

/// One raster's heights, as each cell's trace reads them.
struct Trace<'a> {
    /// Row by row, NaN where the cell is no data or masked.
    heights: &'a [f32],
    w: usize,
    h: usize,
    /// The highest cell, scaled.
    top: f64,
    z_factor: f64,
}

impl Trace<'_> {
    /// The height of the cell at index `i`, scaled, unless it is no data or masked.
    fn height(&self, i: usize) -> Option<f64> {
        self.heights.get(i).filter(|v| !v.is_nan()).map(|v| f64::from(*v) * self.z_factor)
    }

    fn at(&self, col: usize, row: usize) -> Option<f64> {
        self.height(row * self.w + col)
    }

    /// The height between cell centres at column `x` and row `y` (fractional, inside the grid),
    /// bilinear between the four around it; with one of them missing, the nearest one.
    fn between(&self, x: f64, y: f64) -> Option<f64> {
        // Truncating is flooring here (both are at least 0), and much cheaper.
        let (i, j) = (x as usize, y as usize);
        let (fx, fy) = (x - i as f64, y - j as f64);
        let (i1, j1) = ((i + 1).min(self.w - 1), (j + 1).min(self.h - 1));
        match (self.at(i, j), self.at(i1, j), self.at(i, j1), self.at(i1, j1)) {
            (Some(a), Some(b), Some(c), Some(d)) => {
                let top = a + (b - a) * fx;
                let bottom = c + (d - c) * fx;
                Some(top + (bottom - top) * fy)
            }
            _ => self.at(if fx < 0.5 { i } else { i1 }, if fy < 0.5 { j } else { j1 }),
        }
    }

    /// The steepest tangent up to the terrain seen along `way` from the cell at column `col`, row
    /// `row` and height `z0` (`-∞` with nothing seen). The trace stops once nothing farther can
    /// change what it is for: the terrain left can't rise above the tangent kept or `floor`, or the
    /// tangent is already `ceiling`.
    fn horizon(&self, way: &Way, (col, row, z0): (usize, usize, f64), floor: f64, ceiling: f64) -> f64 {
        let (sx, sy) = way.step;
        let (wmax, hmax) = ((self.w - 1) as f64, (self.h - 1) as f64);
        let mut steepest = f64::NEG_INFINITY;
        let mut k = 1.0;
        loop {
            let (x, y) = (col as f64 + sx * k, row as f64 - sy * k);
            let d = k * way.metres;
            if !(0.0..=wmax).contains(&x) || !(0.0..=hmax).contains(&y) {
                break;
            }
            if let Some(z) = self.between(x, y) {
                steepest = steepest.max((z - z0) / d);
            }
            if (self.top - z0) / d <= steepest.max(floor) || steepest >= ceiling {
                break;
            }
            k += 1.0;
        }
        steepest
    }

    /// The steepest tangent, at least 0, up to the terrain seen from the cell at column `col`, row
    /// `row` and height `z0` through `reads` (see [`reads`]): [`Trace::horizon`] for the sky, its
    /// reads worked out once for every cell. The trace stops at the raster's edge, or once the
    /// terrain left can't rise above the tangent kept.
    fn skyline(&self, reads: &[Read], col: usize, row: usize, z0: f64) -> f64 {
        let (w, h) = (self.w as isize, self.h as isize);
        let mut steepest = 0.0f64;
        for read in reads {
            let (i, j) = (col as isize + read.dx, row as isize + read.dy);
            let next = isize::from(read.weight > 0.0);
            let (i1, j1) = if read.along_row { (i + next, j) } else { (i, j + next) };
            if i < 0 || j < 0 || i1 >= w || j1 >= h {
                break;
            }
            let z = match (self.at(i as usize, j as usize), self.at(i1 as usize, j1 as usize)) {
                (Some(a), Some(b)) => Some(a + (b - a) * read.weight),
                (a, b) => {
                    if read.weight < 0.5 {
                        a
                    } else {
                        b
                    }
                }
            };
            if let Some(z) = z {
                steepest = steepest.max((z - z0) * read.per_metre);
            }
            if (self.top - z0) * read.per_metre <= steepest {
                break;
            }
        }
        steepest
    }
}

#[cfg(test)]
mod tests {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform};

    use super::*;

    const CELL: f64 = 30.0;

    fn dem(w: u32, h: u32, z: impl Fn(f64, f64) -> f64) -> Raster {
        let gt = GeoTransform { origin_x: 500_000.0, origin_y: 7_000_000.0, pixel_width: CELL, pixel_height: -CELL };
        let mut data = Vec::with_capacity((w * h) as usize);
        for row in 0..h {
            for col in 0..w {
                // Metres east and north of the north-west corner's cell centre.
                data.push(z(f64::from(col) * CELL, -f64::from(row) * CELL) as f32);
            }
        }
        Raster::new("dem", w, h, vec![data], gt, Crs::from_epsg(31983).unwrap_or(Crs::WebMercator), None, ColorHint::Gray, "Float32")
            .unwrap_or_else(|e| panic!("{e}"))
    }

    fn lit(r: &Raster, azimuth: f64, altitude: f64) -> Vec<u8> {
        light(r, 0, &RasterMask::default(), azimuth, altitude, 1.0, 4, &AtomicBool::new(false)).unwrap_or_else(|e| panic!("{e:?}"))
    }

    fn sky_of(r: &Raster, threads: usize) -> Vec<u8> {
        sky(r, 0, &RasterMask::default(), 1.0, threads, &AtomicBool::new(false)).unwrap_or_else(|e| panic!("{e:?}"))
    }

    /// A plane rising toward the sun shades itself only when it is steeper than the sun is high
    /// (past the soft edge): the same at every azimuth, the diagonals too (surtgis#153).
    #[test]
    fn a_plane_facing_away_is_shaded_only_past_the_suns_altitude() {
        for azimuth in [0.0, 20.0, 45.0, 90.0, 135.0, 180.0, 225.0, 270.0, 300.0, 315.0] {
            let (sin, cos) = f64::to_radians(azimuth).sin_cos();
            for (slope, want) in [(20.0, 255), (24.0, 255), (36.0, 0), (40.0, 0)] {
                let t = f64::to_radians(slope).tan();
                let r = dem(40, 40, |e, n| (e * sin + n * cos) * t);
                let g = lit(&r, azimuth, 30.0);
                // Away from the edges facing the sun, where the trace leaves the raster at once.
                for row in 10..30 {
                    for col in 10..30 {
                        assert_eq!(g[row * 40 + col], want, "azimuth {azimuth}, slope {slope}, cell {col},{row}");
                    }
                }
            }
        }
    }

    /// Inside the soft edge, the light falls with the slope, by the share of the disc hidden.
    #[test]
    fn the_soft_edge_is_the_share_of_the_disc_hidden() {
        let t = f64::to_radians(30.0).tan();
        let r = dem(40, 40, |e, _| e * t);
        // Rising east, lit from the east at the slope's own angle: half the disc is hidden.
        let g = lit(&r, 90.0, 30.0);
        assert!((127..=128).contains(&g[20 * 40 + 20]), "{}", g[20 * 40 + 20]);
    }

    /// A wall casts a shadow as long as its height over the sun's tangent, at each main azimuth.
    #[test]
    fn a_walls_shadow_has_the_length_its_height_gives() {
        let (height, altitude) = (300.0, 45.0);
        // Lit from the east, a north–south wall at column 40 shades the cells west of it.
        let r = dem(60, 20, |e, _| if (e / CELL).round() == 40.0 { height } else { 0.0 });
        let g = lit(&r, 90.0, altitude);
        let row = &g[10 * 60..11 * 60];
        // The whole disc is hidden up to height / tan(45° + 5°) away, none of it past
        // height / tan(45° − 5°).
        let full = (height / f64::to_radians(altitude + SOFT_EDGE).tan() / CELL).floor() as usize;
        let none = (height / f64::to_radians(altitude - SOFT_EDGE).tan() / CELL).ceil() as usize;
        for d in 1..=full {
            assert_eq!(row[40 - d], 0, "{d} cells west");
        }
        for d in none + 1..30 {
            assert_eq!(row[40 - d], 255, "{d} cells west");
        }
        // East of the wall, facing the sun, nothing is shaded.
        assert!(row[41..].iter().all(|v| *v == 255));
    }

    /// A trace stopped part-way gives no grid; flat ground, no data and a mask cast nothing.
    #[test]
    fn stopped_flat_and_missing_cells() {
        let flat = dem(300, 300, |_, _| 100.0);
        let stop = AtomicBool::new(true);
        assert_eq!(light(&flat, 0, &RasterMask::default(), 315.0, 30.0, 1.0, 4, &stop), Err(Unlit::Stopped));
        assert_eq!(sky(&flat, 0, &RasterMask::default(), 1.0, 4, &stop), Err(Unlit::Stopped));
        assert!(lit(&flat, 315.0, 30.0).iter().all(|v| *v == 255));
        assert!(sky_of(&flat, 4).iter().all(|v| *v == 255), "flat ground sees the whole sky");
        // A tower masked out casts no shadow and hides no sky.
        let tower = dem(40, 40, |e, n| if (e / CELL).round() == 20.0 && (n / CELL).round() == -20.0 { 1000.0 } else { 0.0 });
        assert!(lit(&tower, 90.0, 30.0).contains(&0));
        assert!(sky_of(&tower, 1).iter().any(|v| *v < 255));
        assert_eq!(sky_of(&tower, 1)[20 * 40 + 20], 255, "its top sees the whole sky");
        let masked = RasterMask { values: vec![1000.0], ..RasterMask::default() };
        let g = light(&tower, 0, &masked, 90.0, 30.0, 1.0, 1, &AtomicBool::new(false)).unwrap_or_default();
        assert!(g.iter().all(|v| *v == 255));
        let g = sky(&tower, 0, &masked, 1.0, 1, &AtomicBool::new(false)).unwrap_or_default();
        assert!(g.iter().all(|v| *v == 255));
        // A band that isn't there leaves every cell lit.
        let g = light(&tower, 3, &RasterMask::default(), 90.0, 30.0, 1.0, 1, &AtomicBool::new(false)).unwrap_or_default();
        assert!(g.iter().all(|v| *v == 255));
        let g = sky(&tower, 3, &RasterMask::default(), 1.0, 1, &AtomicBool::new(false)).unwrap_or_default();
        assert!(g.iter().all(|v| *v == 255));
    }

    /// The grids are the same on one thread as on several, bit for bit.
    #[test]
    fn threads_give_the_same_grid() {
        let r = dem(300, 300, |e, n| 400.0 * ((e / 900.0).sin() * (n / 700.0).cos()) + e * 0.05);
        let one = light(&r, 0, &RasterMask::default(), 315.0, 25.0, 1.0, 1, &AtomicBool::new(false));
        let many = light(&r, 0, &RasterMask::default(), 315.0, 25.0, 1.0, 7, &AtomicBool::new(false));
        assert_eq!(one, many);
        assert!(one.unwrap_or_default().iter().any(|v| *v < 255));
        let one = sky_of(&r, 1);
        assert_eq!(one, sky_of(&r, 7));
        assert!(one.iter().any(|v| *v < 200));
    }

    /// The bottom of a round pit sees the sky its depth gives: in every direction, the horizon is
    /// the rim, `depth / radius` up, to within the cell the rim is drawn to; the sky is the share
    /// of the half sphere above that, `1 − sin γ`.
    #[test]
    fn a_pit_has_the_sky_its_depth_gives() {
        const RADIUS: f64 = 50.0 * CELL;
        let (n, mid) = (121, 60.0 * CELL);
        for depth in [0.25 * RADIUS, RADIUS, 2.0 * RADIUS] {
            let r = dem(n, n, |e, s| if (e - mid).hypot(s + mid) <= RADIUS { -depth } else { 0.0 });
            let trace = Trace { heights: &r.bands[0].data, w: n as usize, h: n as usize, top: 0.0, z_factor: 1.0 };
            let reach = SKY_REACH * CELL;
            // The rim is between the last cell inside and the first outside: the horizon is there,
            // or a step farther (3 cells at 50), where the trace reads the ground outside whole.
            let (near, far) = (depth / (RADIUS - 0.1 * CELL), depth / (RADIUS + 5.0 * CELL));
            for azimuth in (0..72).map(|i| f64::from(i) * 5.0) {
                let t = trace.skyline(&reads(&Way::new(&r, azimuth), reach, n as usize), 60, 60, -depth);
                assert!((far..=near).contains(&t), "depth {depth}, azimuth {azimuth}: {t} not in {far}..{near}");
            }
            let sin = |t: f64| t / (1.0 + t * t).sqrt();
            let (most, least) = ((1.0 - sin(far)) * 255.0, (1.0 - sin(near)) * 255.0);
            let seen = f64::from(sky_of(&r, 1)[60 * n as usize + 60]);
            assert!((least.floor()..=most.ceil()).contains(&seen), "depth {depth}: {seen} not in {least}..{most}");
        }
    }

    /// Cells too large for their size to be a number of metres (1e308 m, past `f64` once
    /// squared) give no endless trace: the reads stop at the raster's side, and the sky is seen.
    #[test]
    fn huge_cells_end_the_sky_trace() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 0.0, pixel_width: 1e308, pixel_height: -1e308 };
        let data: Vec<f32> = (0..400).map(|i| if i == 210 { 900.0 } else { 0.0 }).collect();
        let r = Raster::new(
            "huge",
            20,
            20,
            vec![data],
            gt,
            Crs::from_epsg(31983).unwrap_or(Crs::WebMercator),
            None,
            ColorHint::Gray,
            "Float32",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let way = Way::new(&r, 30.0);
        assert!(reads(&way, f64::INFINITY, 20).len() <= 20);
        assert!(sky_of(&r, 1).iter().all(|v| *v == 255), "the sky is seen whole");
    }

    /// A tilted plane hides the sky uphill by its slope in each direction, and none of it
    /// downhill: exact at every one of the directions (heights between cells are exact on a plane).
    #[test]
    fn a_planes_sky_is_its_slope_in_each_direction() {
        let slope = f64::to_radians(30.0).tan();
        for uphill in [0.0, 20.0, 45.0, 110.0, 200.0, 315.0] {
            let (sin, cos) = f64::to_radians(uphill).sin_cos();
            let r = dem(60, 60, |e, n| (e * sin + n * cos) * slope);
            let hidden: f64 = (0..SKY_DIRECTIONS)
                .map(|i| {
                    let t = (slope * f64::to_radians(i as f64 * 360.0 / SKY_DIRECTIONS as f64 - uphill).cos()).max(0.0);
                    t / (1.0 + t * t).sqrt()
                })
                .sum();
            let want = ((1.0 - hidden / SKY_DIRECTIONS as f64) * 255.0).round() as u8;
            let g = sky_of(&r, 1);
            for row in 25..35 {
                for col in 25..35 {
                    assert_eq!(g[row * 60 + col], want, "uphill {uphill}, cell {col},{row}");
                }
            }
        }
    }
}
