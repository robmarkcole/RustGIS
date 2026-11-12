//! Cast shadows for Shaded Relief (opengeos/RustGIS#1782): how much of the sun each cell of an
//! elevation raster sees over the terrain between it and the sun.
//!
//! The sun is a disc [`SOFT_EDGE`] degrees either side of its altitude, so a shadow has a soft
//! edge. Each cell traces the terrain toward the sun's azimuth one column or row at a time,
//! reading heights between cells (bilinear: reading the nearest cell instead over-shadows diagonal
//! suns, franciscoparrao/surtgis#153), and keeps the steepest angle up to the terrain it passes.
//! The share of the disc above that angle is the light the cell keeps. The trace stops once
//! nothing farther can change it: the terrain left can't rise above the angle kept (or the disc's
//! lower edge), or the disc is already hidden. Cells outside the raster cast no shadow on it.

use std::sync::atomic::{AtomicBool, Ordering};

use rustgis_data::raster::Raster;

use crate::raster::RasterMask;

/// Half the sun's apparent size, in degrees: how wide a shadow's soft edge is. Far larger than the
/// real sun's quarter of a degree, for a soft edge at map scale.
pub const SOFT_EDGE: f64 = 5.0;

/// The most cells a raster's shadows are worked out for in the browser, which has one thread: past
/// it, the relief is drawn without them, and said.
pub const MAX_BROWSER_CELLS: usize = 500_000;

/// Cells below which the work stays on one thread: starting threads costs more.
const ONE_THREAD_BELOW: usize = 64 * 1024;

/// Why there is no light grid.
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
    let (w, h) = (r.width as usize, r.height as usize);
    let cells = w.checked_mul(h).ok_or(Unlit::NoMemory)?;
    let mut out = Vec::new();
    out.try_reserve_exact(cells).map_err(|_| Unlit::NoMemory)?;
    out.resize(cells, 255u8);
    let Some(data) = r.bands.get(band).map(|b| b.data.as_slice()).filter(|d| d.len() >= cells) else {
        return Ok(out);
    };
    let height = |v: f32| (r.is_valid(v) && !mask.hides(v)).then(|| f64::from(v) * z_factor);
    let top = data.iter().filter_map(|v| height(*v)).fold(f64::NEG_INFINITY, f64::max);
    if !top.is_finite() || w == 0 {
        return Ok(out);
    }
    let sun = Sun::new(r, azimuth, altitude);
    let trace = Trace { data, w, h, top, sun: &sun, height: &height };
    let threads = if cells < ONE_THREAD_BELOW { 1 } else { threads.clamp(1, h) };
    let rows = h.div_ceil(threads);
    let chunks: Vec<(usize, &mut [u8])> = out.chunks_mut(rows * w).enumerate().collect();
    let run = |(i, chunk): (usize, &mut [u8])| -> Result<(), Unlit> {
        for (dy, row) in chunk.chunks_mut(w).enumerate() {
            if stop.load(Ordering::Relaxed) || rustgis_data::progress::check().is_err() {
                return Err(Unlit::Stopped);
            }
            trace.row(i * rows + dy, row);
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

/// The sun as each cell's trace needs it.
struct Sun {
    /// A step toward the sun, in columns and rows: one column or one row, whichever is more.
    step: (f64, f64),
    /// That step's length on the ground, in metres.
    metres: f64,
    /// The tangents of the disc's lower and upper edges.
    low: f64,
    high: f64,
    altitude: f64,
}

impl Sun {
    fn new(r: &Raster, azimuth: f64, altitude: f64) -> Self {
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
        let altitude = altitude.clamp(0.0, 90.0);
        let edge = |a: f64| a.clamp(-89.9, 89.9).to_radians().tan();
        Sun { step, metres, low: edge(altitude - SOFT_EDGE), high: edge(altitude + SOFT_EDGE), altitude }
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

/// One raster's trace toward the sun.
struct Trace<'a, H: Fn(f32) -> Option<f64>> {
    data: &'a [f32],
    w: usize,
    h: usize,
    /// The highest cell, scaled.
    top: f64,
    sun: &'a Sun,
    /// A cell's height, scaled, unless it is no data or masked.
    height: &'a H,
}

impl<H: Fn(f32) -> Option<f64>> Trace<'_, H> {
    fn at(&self, col: usize, row: usize) -> Option<f64> {
        (self.height)(*self.data.get(row * self.w + col)?)
    }

    /// The height between cell centres at column `x` and row `y` (fractional, inside the grid),
    /// bilinear between the four around it; with one of them missing, the nearest one.
    fn between(&self, x: f64, y: f64) -> Option<f64> {
        let (i, j) = (x.floor() as usize, y.floor() as usize);
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

    /// Fills `out` with the light left on each cell of row `row`.
    fn row(&self, row: usize, out: &mut [u8]) {
        let (sx, sy) = self.sun.step;
        let (wmax, hmax) = ((self.w - 1) as f64, (self.h - 1) as f64);
        for (col, cell) in out.iter_mut().enumerate() {
            let Some(z0) = self.at(col, row) else { continue };
            let mut steepest = f64::NEG_INFINITY;
            let mut k = 1.0;
            loop {
                let (x, y) = (col as f64 + sx * k, row as f64 - sy * k);
                if !(0.0..=wmax).contains(&x) || !(0.0..=hmax).contains(&y) {
                    break;
                }
                let d = k * self.sun.metres;
                if let Some(z) = self.between(x, y) {
                    steepest = steepest.max((z - z0) / d);
                }
                // Nothing farther rises above what is kept: the angle seen, or the disc's lower
                // edge (below it the cell has the whole sun); or the whole disc is hidden.
                if (self.top - z0) / d <= steepest.max(self.sun.low) || steepest >= self.sun.high {
                    break;
                }
                k += 1.0;
            }
            *cell = self.sun.lit(steepest);
        }
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
        assert!(lit(&flat, 315.0, 30.0).iter().all(|v| *v == 255));
        // A tower masked out casts no shadow.
        let tower = dem(40, 40, |e, n| if (e / CELL).round() == 20.0 && (n / CELL).round() == -20.0 { 1000.0 } else { 0.0 });
        assert!(lit(&tower, 90.0, 30.0).contains(&0));
        let masked = RasterMask { values: vec![1000.0], ..RasterMask::default() };
        let g = light(&tower, 0, &masked, 90.0, 30.0, 1.0, 1, &AtomicBool::new(false)).unwrap_or_default();
        assert!(g.iter().all(|v| *v == 255));
        // A band that isn't there leaves every cell lit.
        let g = light(&tower, 3, &RasterMask::default(), 90.0, 30.0, 1.0, 1, &AtomicBool::new(false)).unwrap_or_default();
        assert!(g.iter().all(|v| *v == 255));
    }

    /// The grid is the same on one thread as on several, bit for bit.
    #[test]
    fn threads_give_the_same_grid() {
        let r = dem(300, 300, |e, n| 400.0 * ((e / 900.0).sin() * (n / 700.0).cos()) + e * 0.05);
        let one = light(&r, 0, &RasterMask::default(), 315.0, 25.0, 1.0, 1, &AtomicBool::new(false));
        let many = light(&r, 0, &RasterMask::default(), 315.0, 25.0, 1.0, 7, &AtomicBool::new(false));
        assert_eq!(one, many);
        assert!(one.unwrap_or_default().iter().any(|v| *v < 255));
    }
}
