//! Tessera's embeddings, read in place from its Zarr 3 store: a group a UTM zone (`utm17`), with
//! `embeddings` (signed bytes: year, 128 bands, y, x) and `scales` (32-bit floats: year, y, x),
//! the value being their product. Both are sharded: a shard file holds 4 096² pixels in inner
//! chunks of 32² (all 128 bands together), with an index of where each chunk is at its end.
//!
//! A view is read as a window of the zone's grid: the shard index's end of the file, then the
//! inner chunks the window needs (each about 115 kB), within one shard and at most
//! [`MAX_CHUNKS`] chunks; three bands of it are kept, as a 32-bit float raster in the zone's UTM
//! coordinates.

use std::sync::Arc;

use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, zarr};
use rustgis_data::source::remote::{self, Remote};
use rustgis_geom::Coord;
use serde_json::Value as Json;

use super::Tile;

/// The store.
pub const ROOT: &str = "https://data.source.coop/tessera/tessera/zarr/v1.1-dclimate";
/// The years it holds: its `time` axis, one step a year.
pub const FIRST_YEAR: i32 = 2017;
pub const LAST_YEAR: i32 = 2025;
/// Values a pixel.
pub const BANDS: usize = 128;
/// The bands shown first (0-based).
pub const DEFAULT_BANDS: [usize; 3] = [0, 1, 2];
/// A shard's side and an inner chunk's, in pixels.
const SHARD: u64 = 4096;
const INNER: u64 = 32;
/// Inner chunks a shard side.
const PER_SIDE: u64 = SHARD / INNER;
/// A shard's index: an offset and a length a chunk, then its checksum.
const INDEX_LEN: u64 = PER_SIDE * PER_SIDE * 16 + 4;
/// The most inner chunks one read takes (about 115 kB each).
pub const MAX_CHUNKS: usize = if cfg!(target_arch = "wasm32") { 256 } else { 400 };

/// The UTM zone of a longitude (Tessera's groups follow the plain 6° zones).
pub fn zone_of(lon: f64) -> u32 {
    let lon = (lon + 180.0).rem_euclid(360.0) - 180.0;
    (((lon + 180.0) / 6.0).floor() as i64 + 1).clamp(1, 60) as u32
}

/// A zone group's metadata.
pub fn group_url(zone: u32) -> String {
    format!("{ROOT}/utm{zone:02}/zarr.json")
}

/// A zone's grid, from its group's `spatial:` attributes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub zone: u32,
    pub x0: f64,
    pub y0: f64,
    pub dx: f64,
    pub dy: f64,
    pub width: u64,
    pub height: u64,
}

/// A zone's grid from its group's `zarr.json`.
pub fn parse_group(zone: u32, bytes: &[u8]) -> Result<Grid, String> {
    let v: Json = serde_json::from_slice(bytes).map_err(|e| format!("Tessera's zone {zone}: {e}"))?;
    let a = &v["attributes"];
    let t: Vec<f64> = a["spatial:transform"].as_array().map(|t| t.iter().filter_map(Json::as_f64).collect()).unwrap_or_default();
    let shape: Vec<u64> = a["spatial:shape"].as_array().map(|s| s.iter().filter_map(Json::as_u64).collect()).unwrap_or_default();
    match (t.as_slice(), shape.as_slice()) {
        ([dx, 0.0, x0, 0.0, dy, y0], [h, w]) if *dx > 0.0 && *dy < 0.0 && *w > 0 && *h > 0 && x0.is_finite() && y0.is_finite() => {
            Ok(Grid { zone, x0: *x0, y0: *y0, dx: *dx, dy: *dy, width: *w, height: *h })
        }
        _ => Err(format!("Tessera's zone {zone}: its grid isn't one RustGIS reads")),
    }
}

/// What a read takes: a window of a zone's grid (pixels, `col0`, `row0`, `width` × `height`),
/// within one shard, and that shard's inner chunks it needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub grid: Grid,
    pub year: i32,
    pub shard: (u64, u64),
    pub col0: u64,
    pub row0: u64,
    pub width: u64,
    pub height: u64,
    /// Inner chunks (row, column) within the shard.
    pub chunks: Vec<(u64, u64)>,
    /// Whether the window is smaller than the box asked for (a part around its centre).
    pub cut: bool,
}

impl Plan {
    fn shard_key(&self, array: &str) -> String {
        let t = (self.year - FIRST_YEAR).max(0);
        let (sy, sx) = self.shard;
        let zone = self.grid.zone;
        match array {
            "embeddings" => format!("{ROOT}/utm{zone:02}/embeddings/c/{t}/0/{sy}/{sx}"),
            _ => format!("{ROOT}/utm{zone:02}/scales/c/{t}/{sy}/{sx}"),
        }
    }
}

/// The window of a box (longitude/latitude) for a year: the box's pixels in its zone, cut to the
/// shard at its centre and to [`MAX_CHUNKS`] chunks around it. Refuses an unplaceable boundary
/// sample rather than shrinking the requested area to only the surviving samples.
pub fn plan(grid: Grid, bbox: [f64; 4], year: i32) -> Result<Plan, String> {
    if !(FIRST_YEAR..=LAST_YEAR).contains(&year) {
        return Err(format!("Tessera has the years {FIRST_YEAR}–{LAST_YEAR}"));
    }
    let crs = Crs::from_epsg(32600 + grid.zone).ok_or("no such UTM zone")?;
    let t = crs.transformer()?;
    let [w, s, e, n] = super::check_bbox(bbox)?;
    let mut cols = (f64::MAX, f64::MIN);
    let mut rows = (f64::MAX, f64::MIN);
    for (x, y) in [(w, s), (w, n), (e, s), (e, n), ((w + e) / 2.0, s), ((w + e) / 2.0, n)] {
        let c = t
            .from_lonlat(Coord::new(x, y))
            .ok_or_else(|| format!("the area's boundary at ({x}, {y}) can't be placed in Tessera's zone; choose a smaller area"))?;
        let (cx, ry) = ((c.x - grid.x0) / grid.dx, (c.y - grid.y0) / grid.dy);
        cols = (cols.0.min(cx), cols.1.max(cx));
        rows = (rows.0.min(ry), rows.1.max(ry));
    }
    if cols.0 > cols.1 || rows.0 > rows.1 {
        return Err("the area isn't in the zone".into());
    }
    let (wf, hf) = (grid.width as f64, grid.height as f64);
    let (c0, c1) = (cols.0.clamp(0.0, wf), cols.1.clamp(0.0, wf));
    let (r0, r1) = (rows.0.clamp(0.0, hf), rows.1.clamp(0.0, hf));
    if c1 - c0 < 1.0 || r1 - r0 < 1.0 {
        return Err("the area is outside Tessera's grid".into());
    }
    // The shard at the centre, and the window within it.
    let (cc, rc) = (((c0 + c1) / 2.0) as u64, ((r0 + r1) / 2.0) as u64);
    let shard = (rc / SHARD, cc / SHARD);
    let (sx0, sy0) = (shard.1 * SHARD, shard.0 * SHARD);
    let clip = |a: f64, lo: u64| (a as u64).clamp(lo, lo + SHARD);
    let (mut col0, mut col1) = (clip(c0, sx0), clip(c1.ceil(), sx0));
    let (mut row0, mut row1) = (clip(r0, sy0), clip(r1.ceil(), sy0));
    let mut cut = col0 > c0 as u64 || row0 > r0 as u64 || col1 < c1 as u64 || row1 < r1 as u64;
    // At most MAX_CHUNKS chunks: a square of them around the centre.
    // (One chunk less a side: an unaligned window meets one more.)
    let side = ((MAX_CHUNKS as f64).sqrt().floor() as u64).saturating_sub(1).max(1) * INNER;
    if col1 - col0 > side {
        let mid = cc.clamp(col0 + side / 2, col1 - side / 2);
        (col0, col1) = (mid - side / 2, mid + side / 2);
        cut = true;
    }
    if row1 - row0 > side {
        let mid = rc.clamp(row0 + side / 2, row1 - side / 2);
        (row0, row1) = (mid - side / 2, mid + side / 2);
        cut = true;
    }
    if col1 <= col0 || row1 <= row0 {
        return Err("the area is outside Tessera's grid".into());
    }
    let (ic0, ic1) = ((col0 - sx0) / INNER, (col1 - sx0).div_ceil(INNER).min(PER_SIDE));
    let (ir0, ir1) = ((row0 - sy0) / INNER, (row1 - sy0).div_ceil(INNER).min(PER_SIDE));
    let chunks: Vec<(u64, u64)> = (ir0..ir1).flat_map(|r| (ic0..ic1).map(move |c| (r, c))).collect();
    Ok(Plan { grid, year, shard, col0, row0, width: col1 - col0, height: row1 - row0, chunks, cut })
}

/// The zones a box meets, as search results (a window of each read where the map is): the three
/// nearest the box's centre, and whether that is all of them.
pub fn tiles(bbox: [f64; 4], year: i32) -> (Vec<Tile>, bool) {
    let Ok([w, s, e, n]) = super::check_bbox(bbox) else { return (Vec::new(), true) };
    let (z0, z1) = (zone_of(w), zone_of(e.min(179.999)));
    let mut zones: Vec<u32> = (z0..=z1.max(z0)).collect();
    let centre = zone_of((w + e) / 2.0);
    zones.sort_by_key(|z| z.abs_diff(centre));
    let all = zones.len() <= 3;
    zones.truncate(3);
    zones.sort_unstable();
    let tiles = zones
        .into_iter()
        .map(|zone| {
            let zw = -180.0 + f64::from(zone - 1) * 6.0;
            let b = [w.max(zw), s, e.min(zw + 6.0), n];
            Tile {
                dataset: "tessera",
                id: format!("utm{zone:02}"),
                title: format!("Tessera {year} · UTM {zone}"),
                detail: format!("EPSG:{} · 128 bands · 10 m · {year}", 32600 + zone),
                url: format!("{ROOT}/utm{zone:02}"),
                year: Some(year),
                bbox: b,
                footprint: Vec::new(),
            }
        })
        .collect();
    (tiles, all)
}

/// Reads a plan's inner chunks of a shard (`bytes` decoded bytes each), `None` for a chunk the
/// shard leaves out. Fails with [`remote::NEED`] until what it needs is fetched.
fn read_chunks(file: &Remote, plan: &Plan, bytes: usize) -> rustgis_data::Result<Vec<Option<Vec<u8>>>> {
    let (tail, _) = file.tail(INDEX_LEN).ok_or_else(remote::need)?;
    let index = tail.get(tail.len().saturating_sub(INDEX_LEN as usize)..).unwrap_or_default();
    if (index.len() as u64) < INDEX_LEN {
        return Err(rustgis_data::DataError::Format("Tessera: a shard's index is cut".into()));
    }
    let entry = |r: u64, c: u64| -> Option<(u64, u64)> {
        let at = usize::try_from((r * PER_SIDE + c) * 16).ok()?;
        let b = index.get(at..at + 16)?;
        let off = u64::from_le_bytes(b.get(..8)?.try_into().ok()?);
        let len = u64::from_le_bytes(b.get(8..)?.try_into().ok()?);
        (off != u64::MAX && len > 0 && len <= 4 << 20).then_some((off, len))
    };
    let mut held = true;
    for &(r, c) in &plan.chunks {
        if let Some((off, len)) = entry(r, c) {
            held &= file.want(off, len);
        }
    }
    if !held {
        return Err(remote::need());
    }
    plan.chunks
        .iter()
        .map(|&(r, c)| match entry(r, c) {
            Some((off, len)) => {
                let b = file.bytes(off, len).ok_or_else(remote::need)?;
                zarr::blosc_decode(&b, bytes).map(Some)
            }
            None => Ok(None),
        })
        .collect()
}

/// The window's three bands (each embedding × its pixel's scale; NaN where nothing was
/// embedded) as a raster in the zone's UTM coordinates.
pub fn assemble(plan: &Plan, bands: [usize; 3], emb: &[Option<Vec<u8>>], scales: &[Option<Vec<u8>>]) -> Result<Raster, String> {
    let (w, h) = (plan.width as usize, plan.height as usize);
    let n = w.checked_mul(h).filter(|n| *n <= 4 << 20).ok_or("the window is too large")?;
    let mut out = vec![vec![f32::NAN; n]; 3];
    let (sx0, sy0) = (plan.shard.1 * SHARD, plan.shard.0 * SHARD);
    let cell = (INNER * INNER) as usize;
    for (k, &(r, c)) in plan.chunks.iter().enumerate() {
        let (Some(Some(e)), Some(Some(s))) = (emb.get(k), scales.get(k)) else { continue };
        for y in 0..INNER {
            let row = sy0 + r * INNER + y;
            if row < plan.row0 || row >= plan.row0 + plan.height {
                continue;
            }
            for x in 0..INNER {
                let col = sx0 + c * INNER + x;
                if col < plan.col0 || col >= plan.col0 + plan.width {
                    continue;
                }
                let p = (y * INNER + x) as usize;
                let scale = s.get(p * 4..p * 4 + 4).and_then(|b| b.try_into().ok()).map(f32::from_le_bytes).unwrap_or(f32::NAN);
                if !scale.is_finite() {
                    continue;
                }
                let at = (row - plan.row0) as usize * w + (col - plan.col0) as usize;
                for (band, slot) in bands.iter().zip(out.iter_mut()) {
                    let v = e.get((*band).min(BANDS - 1) * cell + p).map(|b| f32::from(*b as i8));
                    if let (Some(v), Some(o)) = (v, slot.get_mut(at)) {
                        *o = v * scale;
                    }
                }
            }
        }
    }
    let g = plan.grid;
    let transform = GeoTransform {
        origin_x: g.x0 + plan.col0 as f64 * g.dx,
        origin_y: g.y0 + plan.row0 as f64 * g.dy,
        pixel_width: g.dx,
        pixel_height: g.dy,
    };
    let crs = Crs::from_epsg(32600 + g.zone).ok_or("no such UTM zone")?;
    let label = bands.iter().map(|b| (b + 1).to_string()).collect::<Vec<_>>().join(", ");
    let mut r = Raster::new(
        format!("Tessera {} ({label})", plan.year),
        plan.width as u32,
        plan.height as u32,
        out,
        transform,
        crs,
        None,
        ColorHint::Rgb { alpha: false },
        "Float32",
    )
    .map_err(|e| e.to_string())?;
    r.source = Some(format!("{ROOT}/utm{:02}", g.zone));
    Ok(r)
}

type Chunks = Vec<Option<Vec<u8>>>;

/// Reads a plan's window (off the caller's thread): the embeddings' chunks, then the scales'.
pub fn read(plan: Plan, bands: [usize; 3], done: Box<dyn FnOnce(Result<Raster, String>) + Send>) {
    let plan = Arc::new(plan);
    let emb_file = remote::file(&plan.shard_key("embeddings"));
    let (f, p) = (emb_file.clone(), plan.clone());
    remote::drive(
        emb_file,
        Arc::new(move || read_chunks(&f, &p, (INNER * INNER) as usize * BANDS)),
        Box::new(move |emb: rustgis_data::Result<Chunks>| {
            let emb = match emb {
                Ok(e) => e,
                Err(e) => return done(Err(e.to_string())),
            };
            let scale_file = remote::file(&plan.shard_key("scales"));
            let (f, p) = (scale_file.clone(), plan.clone());
            remote::drive(
                scale_file,
                Arc::new(move || read_chunks(&f, &p, (INNER * INNER) as usize * 4)),
                Box::new(move |scales: rustgis_data::Result<Chunks>| {
                    done(scales.map_err(|e| e.to_string()).and_then(|s| assemble(&plan, bands, &emb, &s)))
                }),
            );
        }),
    );
}

/// Reads three bands of a box for a year, waiting for it (the command line): the zone's grid,
/// then the window.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_blocking(bbox: [f64; 4], year: i32, bands: [usize; 3]) -> Result<(Raster, bool), String> {
    let bbox = super::check_bbox(bbox)?;
    let zone = zone_of((bbox[0] + bbox[2]) / 2.0);
    let grid = parse_group(zone, &crate::cache::download_blocking(&group_url(zone))?)?;
    let plan = plan(grid, bbox, year)?;
    let cut = plan.cut;
    let (tx, rx) = std::sync::mpsc::channel();
    read(plan, bands, Box::new(move |r| tx.send(r).unwrap_or_default()));
    let r = rx.recv_timeout(std::time::Duration::from_secs(300)).unwrap_or_else(|_| Err("Tessera: no answer in five minutes".into()))?;
    Ok((r, cut))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        parse_group(
            17,
            br#"{"attributes": {"spatial:transform": [10, 0, 163840.0, 0, -10, 9338880.0], "spatial:shape": [1826816, 69632]}}"#,
        )
        .unwrap()
    }

    #[test]
    fn grids_and_windows() {
        assert_eq!(zone_of(-83.92), 17);
        assert_eq!(zone_of(179.99), 60);
        assert!(parse_group(1, b"{}").is_err() && parse_group(1, b"junk").is_err());
        let g = grid();
        // Knoxville, a little over a kilometre: one shard, a few chunks.
        let p = plan(g, [-83.93, 35.96, -83.92, 35.97], 2024).unwrap();
        assert_eq!(p.shard, (130, 1));
        assert!(!p.cut && !p.chunks.is_empty() && p.chunks.len() < 30);
        assert!(p.width > 80 && p.width < 120 && p.height > 100 && p.height < 130);
        assert_eq!(p.shard_key("embeddings"), format!("{ROOT}/utm17/embeddings/c/7/0/130/1"));
        assert_eq!(p.shard_key("scales"), format!("{ROOT}/utm17/scales/c/7/130/1"));
        // A large view: a window of at most MAX_CHUNKS chunks, said to be cut.
        let p = plan(g, [-84.5, 35.5, -83.5, 36.5], 2024).unwrap();
        assert!(p.cut && p.chunks.len() <= MAX_CHUNKS);
        assert!(plan(g, [-83.93, 35.96, -83.92, 35.97], 2016).is_err());
        assert!(plan(g, [100.0, 35.0, 101.0, 36.0], 2024).is_err());
        // South of the equator: the zone's group spans both hemispheres in EPSG:326xx, its
        // northings negative there (spatial:bbox from −8 929 280 m). Cape Town, zone 34.
        let south = Grid { zone: 34, ..g };
        let p = plan(south, [18.42, -33.93, 18.43, -33.92], 2024).unwrap();
        let row = 9_338_880.0 - (-3_755_000.0);
        assert!(((p.row0 as f64) * 10.0 - row).abs() < 5_000.0, "{}", p.row0);
        assert!(!p.cut && !p.chunks.is_empty());
        let (t, all) = tiles([-84.0, 35.0, -77.0, 36.0], 2024);
        assert_eq!(t.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["utm17", "utm18"]);
        assert!(all);
        // Six zones: the three around the centre, and not all of them.
        let (t, all) = tiles([-90.0, 35.0, -54.0, 36.0], 2024);
        assert_eq!(t.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["utm18", "utm19", "utm20"]);
        assert!(!all);
    }

    /// A request with refused corners must not become a partial window from the surviving ones.
    #[test]
    fn a_refused_view_is_not_a_partial_window() {
        let error = plan(grid(), [-170.0, 0.0, -80.0, 40.0], 2024).unwrap_err();
        assert!(error.contains("can't be placed"), "{error}");
    }

    #[test]
    fn windows_assembled() {
        let g = grid();
        let p = plan(g, [-83.93, 35.96, -83.92, 35.97], 2024).unwrap();
        let cell = (INNER * INNER) as usize;
        // Every chunk: band b of a pixel is b − 64; scales 0.5, but the first pixel's NaN.
        let emb: Vec<Option<Vec<u8>>> =
            p.chunks.iter().map(|_| Some((0..BANDS * cell).map(|i| ((i / cell) as i32 - 64) as i8 as u8).collect())).collect();
        let mut sc: Vec<u8> = (0..cell).flat_map(|_| 0.5f32.to_le_bytes()).collect();
        sc[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        let scales: Vec<Option<Vec<u8>>> = p.chunks.iter().map(|_| Some(sc.clone())).collect();
        let r = assemble(&p, [0, 10, 127], &emb, &scales).unwrap();
        assert_eq!((r.width as u64, r.height as u64, r.bands.len()), (p.width, p.height, 3));
        let px = |b: usize, i: usize| r.bands[b].data[i];
        let i = r.width as usize + 1;
        assert_eq!((px(0, i), px(1, i), px(2, i)), (-32.0, -27.0, 31.5));
        assert_eq!(r.crs.epsg(), 32617);
        // Chunks missing, short or junk: blank, never a panic.
        let none: Vec<Option<Vec<u8>>> = vec![None; p.chunks.len()];
        // Nothing embedded: a raster of no valid pixels (the add says there are none).
        let blank = assemble(&p, [0, 1, 2], &none, &none).unwrap();
        assert!(blank.bands.iter().all(|b| b.stats.count == 0));
        let short: Vec<Option<Vec<u8>>> = p.chunks.iter().map(|_| Some(vec![1, 2, 3])).collect();
        let _ = assemble(&p, [0, 1, 2], &short, &short);
    }
}
