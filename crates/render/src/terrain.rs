//! A map's terrain (`ViewSettings::terrain`): the height of the ground under a point, from a DEM
//! raster layer or from elevation tiles (Terrarium or Mapbox Terrain-RGB), for a tilted view to
//! drape the map over (as MapLibre's terrain).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use rustgis_data::raster::Raster;
use rustgis_geom::{Coord, MERCATOR_R};
use rustgis_map::view::{TerrainEncoding, TerrainSource};
use rustgis_map::{Camera, Map};
use rustgis_tiles::cache::{self, Payload, State, Want};
use rustgis_tiles::template::{self, TileId};

/// Cells along each side of a tilted view's band laid over the relief while the map moves (the
/// drape, `tilt_drape` in the app): few enough to lay out every frame.
pub const CELLS: usize = 40;

/// The fewest cells along each side of a band laid over the relief in the frame that stays on
/// screen once the map settles, and in exports (#1523; [`settled_cells`] grows it with the
/// picture, #1882): fine enough that a ridge's outline follows the ridge, where 40 drew it in long
/// straight lines and let a massif sink into its surroundings.
pub const SETTLED_CELLS: usize = 4 * CELLS;

/// The most cells along each side of a band laid over the relief in the settled frame and in
/// exports (#1882): 1280 showed no more of a ridge than 640, for about three times the memory.
pub const MAX_SETTLED_CELLS: usize = 4 * SETTLED_CELLS;

/// Pixels of a band's picture to each of its mesh's cells in the settled frame and in exports
/// (#1882): a finer mesh outruns the picture draped over it.
const PX_PER_SETTLED_CELL: u32 = 4;

/// Cells along each side of a band laid over the relief in the settled frame and in exports
/// (#1882): one every [`PX_PER_SETTLED_CELL`] pixels of the longer side of the band's picture,
/// `picture_px`, and no more than `dem_cells`, the ground's own cells across the band
/// ([`Ground::cells_across`], `None` when unknown); never fewer than [`SETTLED_CELLS`] (no
/// coarser than before) nor more than [`MAX_SETTLED_CELLS`].
pub fn settled_cells(picture_px: u32, dem_cells: Option<f64>) -> usize {
    let mut n = (picture_px / PX_PER_SETTLED_CELL) as usize;
    if let Some(d) = dem_cells.filter(|d| d.is_finite()) {
        n = n.min(d.max(0.0).ceil() as usize);
    }
    n.clamp(SETTLED_CELLS, MAX_SETTLED_CELLS)
}

/// The deepest zoom of elevation tiles read.
const MAX_TILE_ZOOM: u8 = 15;
/// How many zooms up a missing tile is stood in for by a coarser one already read.
const STAND_IN_LEVELS: u8 = 8;
/// How many zooms up the one coarser tile fetched along with a missing one is: cheap, and soon
/// there, so the relief shows while the sharper tile comes.
const COARSE_LEVELS: u8 = 4;

enum Heights {
    Raster {
        raster: Arc<Raster>,
        wrap_width: Option<i64>,
    },
    Tiles {
        url: String,
        encoding: TerrainEncoding,
        zoom: u8,
        /// The tiles read so far, and whether one is still on its way.
        read: RefCell<HashMap<TileId, Option<Arc<Payload>>>>,
        pending: RefCell<bool>,
        /// Tiles found on their way, not asked for again while this view is drawn.
        waiting: RefCell<std::collections::HashSet<TileId>>,
    },
}

/// The ground of a map's terrain, read for one view.
pub struct Ground {
    heights: Heights,
    exaggeration: f64,
    /// The map's body's ground over the Earth's (#223): heights are metres on it.
    body: f64,
}

impl Ground {
    /// The terrain of `map` as a view of `camera` sees it; `None` when the map has none (or
    /// isn't drawn in Web Mercator, which the heights are looked up in), or its DEM layer isn't
    /// there.
    pub fn of(map: &Map, camera: &Camera) -> Option<Ground> {
        let t = map.view.terrain.as_ref()?;
        if map.projection != rustgis_map::Projection::WebMercator {
            return None;
        }
        let exaggeration = if t.exaggeration.is_finite() { t.exaggeration.clamp(0.0, rustgis_map::view::MAX_EXAGGERATION) } else { 1.0 };
        let heights = match &t.source {
            TerrainSource::Layer(name) => {
                let raster = map.layer_by_name(name)?.raster.as_ref()?.data.clone()?;
                let wrap_width = crate::raster::full_turn_width(&raster);
                Heights::Raster { raster, wrap_width }
            }
            TerrainSource::Tiles { url, encoding } => {
                // Tiles a few screen pixels a cell at the view's centre: relief, not detail.
                let world = 2.0 * std::f64::consts::PI * MERCATOR_R;
                let z = (world / (256.0 * camera.res.max(1e-9) * 4.0)).log2();
                let zoom = if z.is_finite() { z.round().clamp(0.0, f64::from(MAX_TILE_ZOOM)) as u8 } else { 0 };
                Heights::Tiles {
                    url: url.clone(),
                    encoding: *encoding,
                    zoom,
                    read: RefCell::new(HashMap::new()),
                    pending: RefCell::new(false),
                    waiting: RefCell::new(std::collections::HashSet::new()),
                }
            }
        };
        Some(Ground { heights, exaggeration, body: map.body().ratio() })
    }

    /// The ground's height (metres, exaggerated) at a Web Mercator point; `None` where nothing
    /// is known (off the DEM, or no tile read yet).
    pub fn height(&self, m: Coord) -> Option<f64> {
        let h = match &self.heights {
            Heights::Raster { raster, wrap_width } => raster_height(raster, m, *wrap_width)?,
            Heights::Tiles { url, encoding, zoom, read, pending, waiting } => {
                tile_height(url, *encoding, *zoom, read, pending, waiting, m)?
            }
        };
        let h = h * self.exaggeration;
        h.is_finite().then_some(h)
    }

    /// How high the ground stands at `m` in a view of `cam`, in logical pixels at the centre's
    /// scale (what `Camera::project_raised` takes): its height over the metres a map unit spans
    /// there (Web Mercator stretches the ground toward the poles). 0 where nothing is known.
    pub fn up_px(&self, m: Coord, cam: &Camera) -> f64 {
        let Some(h) = self.height(m) else { return 0.0 };
        (h / self.per_unit(m) / cam.res.max(1e-9)).clamp(-1e7, 1e7)
    }

    /// The ground metres a map unit spans at `m` on the map's body (Web Mercator stretches the
    /// ground toward the poles): a height over it is in map units.
    pub fn per_unit(&self, m: Coord) -> f64 {
        per_unit(m, self.body)
    }

    /// How many of the ground's own cells span the stretch of ground with these `corners` (Web
    /// Mercator), along the longer side of their box in the ground's grid: a DEM's cells, the
    /// box clipped to the DEM (a geographic DEM's columns met again a whole turn round, as
    /// [`raster_height`] reads them; a whole turn's not clipped at all); elevation tiles'
    /// pixels at the zoom read. A turned stretch's box is up to √2 longer than its side.
    /// `None` when no corner lands on the grid's plane.
    pub fn cells_across(&self, corners: &[Coord]) -> Option<f64> {
        // `turn`: a geographic DEM's columns in 360°, where `raster_height` meets it again.
        let (pixels, (w, h), full_turn, turn): (Vec<(f64, f64)>, _, _, _) = match &self.heights {
            Heights::Raster { raster, wrap_width } => {
                let t = raster.transformer();
                let pixels = corners.iter().filter_map(|m| t.from_merc(*m)).map(|n| raster.transform.to_pixel(n)).collect();
                let turn = t.is_geographic().then(|| 360.0 / raster.transform.pixel_width.abs()).filter(|t| t.is_finite());
                (pixels, (f64::from(raster.width), f64::from(raster.height)), wrap_width.is_some(), turn)
            }
            Heights::Tiles { zoom, .. } => {
                let half = std::f64::consts::PI * MERCATOR_R;
                let n = 256.0 * f64::from(1u32 << (*zoom).min(30));
                let pixels = corners.iter().map(|m| ((m.x + half) / (2.0 * half) * n, (half - m.y) / (2.0 * half) * n)).collect();
                (pixels, (n, n), false, None)
            }
        };
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (x, y) in pixels.into_iter().filter(|(x, y)| x.is_finite() && y.is_finite()) {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        if x0 > x1 {
            return None;
        }
        let across = |a: f64, b: f64, size: f64| (b.min(size) - a.max(0.0)).max(0.0);
        let x = match turn {
            // A whole turn's columns go on past either edge: no edge to clip at, one turn at most.
            _ if full_turn => (x1 - x0).clamp(0.0, w),
            // Moved round by the whole turns that put most of the stretch on the DEM (a 0–360°
            // grid, a stretch across the seam from a DEM beside it, a view a turn away).
            Some(t) => {
                let k = ((w - x0 - x1) / 2.0 / t).round();
                [k - 1.0, k, k + 1.0].iter().map(|k| across(x0 + k * t, x1 + k * t, w)).fold(0.0, f64::max)
            }
            None => across(x0, x1, w),
        };
        Some(x.max(across(y0, y1, h)))
    }

    /// Whether elevation tiles are still on their way (drawing again later shows more).
    pub fn pending(&self) -> bool {
        match &self.heights {
            Heights::Raster { .. } => false,
            Heights::Tiles { pending, .. } => *pending.borrow(),
        }
    }
}

/// A DEM's height at a Web Mercator point, read bilinearly from its first band; nodata and
/// points off it give `None`.
fn raster_height(r: &Raster, m: Coord, wrap_width: Option<i64>) -> Option<f64> {
    let n = r.transformer().from_merc(m)?;
    let (px, py) = r.transform.to_pixel(n);
    let px = r.wrap_col(px);
    let (fx, fy) = (px - 0.5, py - 0.5);
    if !(fx.is_finite() && fy.is_finite()) {
        return None;
    }
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let at = |dx: i64, dy: i64| -> Option<f64> {
        let x = x0 as i64 + dx;
        let x = wrap_width.map_or_else(|| x.clamp(0, i64::from(r.width) - 1), |w| if x < 0 || x >= w { x.rem_euclid(w) } else { x });
        let y = (y0 as i64 + dy).clamp(0, i64::from(r.height) - 1);
        r.value(0, x, y).map(f64::from).filter(|v| v.is_finite())
    };
    // Off the grid by more than half a cell: nothing.
    if px < 0.0 || py < 0.0 || px > f64::from(r.width) || py > f64::from(r.height) {
        return None;
    }
    let v00 = at(0, 0)?;
    let (v10, v01, v11) = (at(1, 0).unwrap_or(v00), at(0, 1).unwrap_or(v00), at(1, 1).unwrap_or(v00));
    let top = v00 + (v10 - v00) * tx;
    let bottom = v01 + (v11 - v01) * tx;
    Some(top + (bottom - top) * ty)
}

/// The tile at `zoom` holding a Web Mercator point, and where in it (0–1 across, down).
fn tile_at(m: Coord, zoom: u8) -> Option<(TileId, f64, f64)> {
    let half = std::f64::consts::PI * MERCATOR_R;
    let n = f64::from(1u32 << zoom.min(30));
    let (fx, fy) = ((m.x + half) / (2.0 * half) * n, (half - m.y) / (2.0 * half) * n);
    if !(fx.is_finite() && fy.is_finite()) || fx < 0.0 || fy < 0.0 || fx >= n || fy >= n {
        return None;
    }
    let id = TileId { z: zoom, x: fx.floor() as u32, y: fy.floor() as u32 };
    Some((id, fx.fract(), fy.fract()))
}

/// An elevation tile's height at a Web Mercator point: the tile at `zoom` once read (its fetch
/// starts here, with one [`COARSE_LEVELS`] up), meanwhile the nearest coarser one read already.
/// A failed fetch is tried again only after the cache's back-off.
fn tile_height(
    url: &str,
    encoding: TerrainEncoding,
    zoom: u8,
    read: &RefCell<HashMap<TileId, Option<Arc<Payload>>>>,
    pending: &RefCell<bool>,
    waiting: &RefCell<std::collections::HashSet<TileId>>,
    m: Coord,
) -> Option<f64> {
    let (id, _, _) = tile_at(m, zoom)?;
    let tile = |id: TileId, fetch: bool| -> Option<Arc<Payload>> {
        if let Some(t) = read.borrow().get(&id) {
            return t.clone();
        }
        if waiting.borrow().contains(&id) {
            return None;
        }
        let u = template::expand(url, id, 256, false, &[]);
        let got = if fetch {
            match cache::get_tile(&u, Want::Image) {
                State::Ready(p) => Some(p),
                State::Pending => {
                    *pending.borrow_mut() = true;
                    // Not asked again while this view is drawn: the next frame, drawn when it
                    // comes, asks again.
                    waiting.borrow_mut().insert(id);
                    return None;
                }
                State::Failed(_) => None,
            }
        } else {
            cache::peek(&u)
        };
        read.borrow_mut().insert(id, got.clone());
        got
    };
    let coarse = COARSE_LEVELS.min(zoom);
    let stand_ins = (0..=STAND_IN_LEVELS.min(zoom)).filter_map(|k| id.ancestor(k));
    for (k, t) in stand_ins.enumerate() {
        if let Some(p) = tile(t, k == 0 || k == usize::from(coarse))
            && let Payload::Image(pm) = &*p
            && let Some((_, fx, fy)) = tile_at(m, t.z)
        {
            return pixel_height(pm, encoding, fx, fy);
        }
    }
    None
}

/// The height an elevation tile's pixels give at (`fx`, `fy`) across it, bilinearly.
fn pixel_height(pm: &tiny_skia::Pixmap, encoding: TerrainEncoding, fx: f64, fy: f64) -> Option<f64> {
    let (w, h) = (pm.width() as i64, pm.height() as i64);
    if w == 0 || h == 0 {
        return None;
    }
    let (x, y) = (fx * w as f64 - 0.5, fy * h as f64 - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (tx, ty) = (x - x0, y - y0);
    let px = pm.pixels();
    let at = |dx: i64, dy: i64| -> Option<f64> {
        let (xi, yi) = ((x0 as i64 + dx).clamp(0, w - 1), (y0 as i64 + dy).clamp(0, h - 1));
        let p = px.get((yi * w + xi) as usize)?.demultiply();
        Some(encoding.height(p.red(), p.green(), p.blue()))
    };
    let (a, b, c, d) = (at(0, 0)?, at(1, 0)?, at(0, 1)?, at(1, 1)?);
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    Some(top + (bottom - top) * ty)
}

/// The ground metres a Web Mercator unit spans at `m` on a body `body` times the Earth's size.
fn per_unit(m: Coord, body: f64) -> f64 {
    (rustgis_map::projection::Flat::Mercator.ground_per_unit(m) * body).max(1e-9)
}

#[cfg(test)]
mod tests {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform};
    use rustgis_geom::lonlat_to_merc;

    use super::*;

    fn raster_ground(pixel_width: f64, crs: Crs, nodata: Option<f64>) -> Ground {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 90.0, pixel_width, pixel_height: -60.0 };
        let data = [0.0, 10.0, 20.0, 30.0].repeat(3);
        let raster = Arc::new(Raster::new("heights", 4, 3, vec![data], gt, crs, nodata, ColorHint::Gray, "Float32").unwrap());
        let wrap_width = crate::raster::full_turn_width(&raster);
        Ground { heights: Heights::Raster { raster, wrap_width }, exaggeration: 1.0, body: 1.0 }
    }

    #[test]
    fn raster_terrain_blends_across_both_ends_of_a_full_turn() {
        for crs in [Crs::LonLat, Crs::from_epsg(4269).unwrap()] {
            for pixel_width in [90.0, -90.0] {
                let ground = raster_ground(pixel_width, crs, None);
                for (col, expected) in [(0.0, 15.0), (0.25, 7.5), (3.75, 22.5)] {
                    let m = lonlat_to_merc(Coord::new(col * pixel_width, 0.0));
                    let h = ground.height(m).unwrap();
                    assert!((h - expected).abs() < 1e-8, "{pixel_width}, column {col}: {h} != {expected}");
                }
                let west = ground.height(lonlat_to_merc(Coord::new(-1e-6, 0.0))).unwrap();
                let east = ground.height(lonlat_to_merc(Coord::new(1e-6, 0.0))).unwrap();
                assert!((west - east).abs() < 1e-5, "{west} vs {east}");
            }
        }
    }

    #[test]
    fn raster_terrain_keeps_regional_projected_and_nodata_edges() {
        for (pixel_width, crs) in [(10.0, Crs::LonLat), (89.9, Crs::LonLat), (90.1, Crs::LonLat), (90.0, Crs::WebMercator)] {
            let ground = raster_ground(pixel_width, crs, None);
            let point = |col| {
                let c = Coord::new(col * pixel_width, 0.0);
                if crs == Crs::WebMercator { c } else { lonlat_to_merc(c) }
            };
            for (col, expected) in [(0.25, 0.0), (3.75, 30.0)] {
                let h = ground.height(point(col)).unwrap();
                assert!((h - expected).abs() < 1e-8, "{pixel_width}, column {col}: {h} != {expected}");
            }
            // A gap just west of the grid remains outside a regional or projected DEM.
            if pixel_width == 10.0 || crs == Crs::WebMercator {
                assert!(ground.height(point(-0.25)).is_none());
            }
        }
        let ground = raster_ground(90.0, Crs::LonLat, Some(30.0));
        assert!(ground.height(lonlat_to_merc(Coord::new(22.5, 0.0))).is_none(), "the wrapped base neighbour is nodata");
        assert!(ground.height(lonlat_to_merc(Coord::new(-22.5, 0.0))).is_none());
    }

    #[test]
    fn the_settled_mesh_follows_the_picture_within_its_bounds() {
        // #1882: one cell every 4 px, within 160 and 640.
        for (px, n) in [(0, 160), (400, 160), (640, 160), (1600, 400), (2000, 500), (2560, 640), (4800, 640), (u32::MAX, 640)] {
            assert_eq!(settled_cells(px, None), n, "{px} px");
        }
        // Never past the DEM's cells across the band, nor coarser than 160.
        assert_eq!(settled_cells(4800, Some(300.0)), 300);
        assert_eq!(settled_cells(4800, Some(299.2)), 300);
        assert_eq!(settled_cells(1600, Some(1924.0)), 400);
        assert_eq!(settled_cells(4800, Some(100.0)), SETTLED_CELLS);
        assert_eq!(settled_cells(4800, Some(0.0)), SETTLED_CELLS);
        assert_eq!(settled_cells(1600, Some(f64::NAN)), 400);
        assert_eq!(settled_cells(4800, Some(f64::INFINITY)), 640);
    }

    #[test]
    fn a_stretch_spans_the_ground_cells_under_it() {
        // A 4 × 3 grid of 90° by 60° cells, the whole Earth: a stretch over two columns spans 2
        // cells, one past its edges only the grid's.
        let ground = raster_ground(90.0, Crs::LonLat, None);
        let at = |lon: f64, lat: f64| lonlat_to_merc(Coord::new(lon, lat));
        let two = ground.cells_across(&[at(0.0, 30.0), at(180.0, 30.0), at(0.0, -30.0), at(180.0, -30.0)]).unwrap();
        assert!((two - 2.0).abs() < 1e-6, "{two}");
        let all = ground.cells_across(&[Coord::new(-1e8, 1e8), Coord::new(1e8, -1e8)]).unwrap();
        assert!(all <= 4.0 + 1e-9, "{all}");
        assert_eq!(ground.cells_across(&[]), None);
        // Across the seam, the whole stretch: 135° to 225° (-135°) spans one cell, not the half
        // cell short of the grid's edge; the same stretch a turn away likewise.
        let seam = ground.cells_across(&[at(135.0, 30.0), at(225.0, 30.0)]).unwrap();
        assert!((seam - 1.0).abs() < 1e-6, "{seam}");
        let away = ground.cells_across(&[at(495.0, 30.0), at(585.0, 30.0)]).unwrap();
        assert!((away - 1.0).abs() < 1e-6, "{away}");
        // A regional DEM of 1° cells beside the seam: a stretch across the seam from it counts
        // the cells it reaches there, as its heights are read, though its middle is off the DEM.
        let regional = |west: f64| {
            let gt = GeoTransform { origin_x: west, origin_y: 90.0, pixel_width: 1.0, pixel_height: -60.0 };
            let raster =
                Arc::new(Raster::new("heights", 10, 3, vec![vec![0.0; 30]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap());
            Ground { heights: Heights::Raster { raster, wrap_width: None }, exaggeration: 1.0, body: 1.0 }
        };
        let east_of_seam = regional(-180.0).cells_across(&[at(170.0, 30.0), at(186.0, 30.0)]).unwrap();
        assert!((east_of_seam - 6.0).abs() < 1e-6, "{east_of_seam}");
        let a_turn_on = regional(0.0).cells_across(&[at(345.0, 30.0), at(365.0, 30.0)]).unwrap();
        assert!((a_turn_on - 5.0).abs() < 1e-6, "{a_turn_on}");
        // Elevation tiles: their pixels at the zoom read; the whole world at zoom 2 is 1024.
        let tiles = Ground {
            heights: Heights::Tiles {
                url: String::new(),
                encoding: TerrainEncoding::Terrarium,
                zoom: 2,
                read: RefCell::new(HashMap::new()),
                pending: RefCell::new(false),
                waiting: RefCell::new(std::collections::HashSet::new()),
            },
            exaggeration: 1.0,
            body: 1.0,
        };
        let half = std::f64::consts::PI * MERCATOR_R;
        let world = tiles.cells_across(&[Coord::new(-2.0 * half, half), Coord::new(2.0 * half, -half)]).unwrap();
        assert!((world - 1024.0).abs() < 1e-6, "{world}");
        // An eighth of the world across, a sixteenth down: the longer side.
        let quarter = tiles.cells_across(&[Coord::new(0.0, 0.0), Coord::new(half / 2.0, -half / 4.0)]).unwrap();
        assert!((quarter - 256.0).abs() < 1e-6, "{quarter}");
    }

    #[test]
    fn heights_stand_in_the_metres_of_the_body() {
        // A metre on Mars spans more of the (Earth-sized) map than one on the Earth (#223).
        let at = Coord::new(0.0, 0.0);
        let mars = rustgis_geom::Body::get("mars").ratio();
        assert!((per_unit(at, 1.0) / per_unit(at, mars) - 1.0 / mars).abs() < 1e-9);
    }

    #[test]
    fn elevation_tiles_decode_their_heights() {
        // Terrarium: 32768 m is zero.
        assert_eq!(TerrainEncoding::Terrarium.height(128, 0, 0), 0.0);
        assert_eq!(TerrainEncoding::Terrarium.height(131, 232, 0), 1000.0);
        // Terrain-RGB: −10000 m plus tenths of a metre.
        assert!((TerrainEncoding::Mapbox.height(1, 134, 160) - 0.0).abs() < 1e-9);
        // A tile's pixels, bilinearly.
        let mut pm = tiny_skia::Pixmap::new(2, 1).unwrap();
        pm.pixels_mut()[0] = tiny_skia::PremultipliedColorU8::from_rgba(128, 0, 0, 255).unwrap();
        pm.pixels_mut()[1] = tiny_skia::PremultipliedColorU8::from_rgba(128, 100, 0, 255).unwrap();
        let mid = pixel_height(&pm, TerrainEncoding::Terrarium, 0.5, 0.5).unwrap();
        assert!((mid - 50.0).abs() < 1e-9, "{mid}");
        // The tile holding a point.
        let (id, fx, fy) = tile_at(Coord::new(0.0, 0.0), 1).unwrap();
        assert_eq!((id.z, id.x, id.y), (1, 1, 1));
        assert!(fx.abs() < 1e-9 && fy.abs() < 1e-9);
        assert!(tile_at(Coord::new(0.0, 1e9), 3).is_none());
    }
}
