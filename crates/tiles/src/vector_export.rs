//! WGS 84 vector features to uncompressed MVT in a PMTiles v3 archive.
//!
//! Input follows [`Dataset`]'s invariant: geometry is already WGS 84 longitude/latitude;
//! `Dataset::crs` records source provenance, not the coordinates' current CRS. Export projects
//! these degrees into Web Mercator independently of the map CRS. Paths take the short way across
//! the antimeridian, then are clipped in an unwrapped world and wrapped into XYZ tiles. The
//! clipping buffer is **8 / 4096 tile units**, including at world edges. Polygon intersection
//! keeps disconnected pieces and holes; quantized rings use MVT's y-down winding convention.
//! This is a bounded in-memory exporter, not a sampler: resource limits reject the whole job.
//! Per-feature property scratch is released after cloning; retained tile properties stay budgeted.
//! Allocation estimates are linear in geometry size. A separate cumulative CPU-work budget
//! bounds quadratic polygon validation and candidate-tile vertex scans; a component's buffered
//! candidate rectangle may not exceed 40,000 tiles at one zoom.

use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;

use geo::{BooleanOps, Coord as GC, LineString, Polygon, Rect, Validation};
use rustgis_data::{Coord, Dataset, FieldType, Geometry, Value};

use crate::{mvt, pmtiles};

const EXTENT: u32 = 4096;
const BUFFER: f64 = 8.0 / EXTENT as f64;
const MAX_LAT: f64 = 85.051_128_779_806_6;
const MAX_MEMORY: usize = 256 << 20;
const MAX_TILES: usize = 10_000;
const MAX_WORK: usize = 100_000_000;
const MEMORY_ERROR: &str = "vector export exceeds the conservative 256 MiB memory limit; reduce the input or zoom range";
const WORK_ERROR: &str = "vector export exceeds the 100,000,000-unit geometry work limit; simplify polygon rings, reduce the input extent, or narrow the zoom range";
type TileKey = (u8, u32, u32);
type Parts = Vec<Vec<[f32; 2]>>;

/// A complete, deliverable archive and notices about deliberate geometry changes.
pub struct Export {
    pub bytes: Vec<u8>,
    pub messages: Vec<String>,
}

#[derive(Default)]
struct Notices {
    poles: usize,
    collapsed: usize,
    missing: usize,
    wrapped: bool,
}

/// Export all rows when `rows` is empty, otherwise exactly the supplied row indices.
///
/// Zooms must satisfy `0 <= min_zoom <= max_zoom <= 16`. At most 10,000 distinct
/// tiles and a conservative 256 MiB peak (including borrowed input, geometry work,
/// encoder temporaries, writer duplication and the final archive) are allowed.
/// Geometry work is capped separately at 100,000,000 cumulative units: one per
/// candidate-tile vertex and `(vertices + rings)^2` per polygon validity check.
/// Integers beyond the exact f64 range are rejected, never silently rounded.
///
/// Geometry must already be normalized to WGS 84 degrees, as required by [`Dataset`].
/// Its `crs` is source provenance and must not be used to transform those degrees again.
pub fn export(dataset: &Dataset, rows: &[usize], min_zoom: u8, max_zoom: u8, source_layer: &str) -> Result<Export, String> {
    if min_zoom > max_zoom || max_zoom > 16 {
        return Err("zoom range must satisfy 0 <= minZoom <= maxZoom <= 16".into());
    }
    if source_layer.trim().is_empty() {
        return Err("source layer ID must not be empty".into());
    }
    if dataset.windowed || dataset.crs_incomplete {
        return Err("export requires a complete, fully georeferenced vector layer".into());
    }
    let mut budget = Budget { used: input_size(dataset)?, work: 0 };
    budget.add(product(rows.len(), size_of::<usize>())?)?;
    budget.add(product(source_layer.len(), 16)?)?;
    let mut names = BTreeSet::new();
    for field in &dataset.fields {
        if field.name.is_empty() || !names.insert(field.name.as_str()) {
            return Err("MVT fields must have nonempty, unique names".into());
        }
        budget.add(field.name.len().checked_add(128).ok_or(MEMORY_ERROR)?)?;
    }
    let mut tiles: BTreeMap<TileKey, Vec<mvt::Feature>> = BTreeMap::new();
    let mut notices = Notices::default();
    let mut bounds = [180.0_f64, MAX_LAT, -180.0_f64, -MAX_LAT];
    let count = if rows.is_empty() { dataset.features.len() } else { rows.len() };
    // Keep row selection borrowed: do not copy all features or synthesize an index vector.
    for at in 0..count {
        let row = if rows.is_empty() { at } else { *rows.get(at).ok_or_else(|| format!("selected row index {at} is out of range"))? };
        let feature = dataset.features.get(row).ok_or_else(|| format!("feature row {row} is out of range"))?;
        let Some(geometry) = &feature.geometry else {
            notices.missing += 1;
            continue;
        };
        let prop_size = dataset.fields.iter().zip(&feature.attrs).try_fold(0usize, |n, (field, value)| {
            let text = if let Value::Text(s) = value { s.len() } else { 16 };
            n.checked_add(field.name.len()).and_then(|n| n.checked_add(text)).and_then(|n| n.checked_add(128)).ok_or(MEMORY_ERROR)
        })?;
        let property_reserve = product(prop_size, 8)?;
        budget.add(property_reserve)?;
        let props = properties(dataset, &feature.attrs)?;
        let mut local: BTreeMap<TileKey, Parts> = BTreeMap::new();
        let kind = match geometry {
            Geometry::Point(_) | Geometry::MultiPoint(_) => mvt::GeomKind::Point,
            Geometry::Line(_) => mvt::GeomKind::LineString,
            Geometry::Polygon(_) => mvt::GeomKind::Polygon,
        };
        match geometry {
            Geometry::Point(c) => {
                let p = project(*c, &mut notices, &mut bounds)?;
                emit(&[vec![p]], kind, min_zoom, max_zoom, &mut local, &mut budget, &mut notices)?;
            }
            Geometry::MultiPoint(points) => {
                for c in points {
                    let p = project(*c, &mut notices, &mut bounds)?;
                    emit(&[vec![p]], kind, min_zoom, max_zoom, &mut local, &mut budget, &mut notices)?;
                }
            }
            Geometry::Line(parts) => {
                for part in parts {
                    budget.ensure(geometry_memory(part.len(), 1, false)?)?;
                    let path = project_path(part, None, &mut notices, &mut bounds)?;
                    emit(&[path], kind, min_zoom, max_zoom, &mut local, &mut budget, &mut notices)?;
                }
            }
            Geometry::Polygon(polygons) => {
                for polygon in polygons {
                    let vertices = polygon.iter().try_fold(0usize, |n, r| n.checked_add(r.len()).ok_or(MEMORY_ERROR))?;
                    budget.ensure(geometry_memory(vertices, polygon.len(), true)?)?;
                    let Some(exterior) = polygon.first() else { continue };
                    let outer = project_path(exterior, None, &mut notices, &mut bounds)?;
                    let anchor = path_centre(&outer);
                    let mut rings = vec![outer];
                    for hole in polygon.iter().skip(1) {
                        rings.push(project_path(hole, anchor, &mut notices, &mut bounds)?);
                    }
                    emit(&rings, kind, min_zoom, max_zoom, &mut local, &mut budget, &mut notices)
                        .map_err(|error| format!("feature {}: {error}", feature.id))?;
                }
            }
        }
        for (key, geometry) in local {
            if !tiles.contains_key(&key) && tiles.len() >= MAX_TILES {
                return Err("vector export exceeds the 10,000 tile limit; reduce the zoom range or input extent".into());
            }
            // Include Vec capacity growth, property cloning, and tree/feature bookkeeping.
            budget.add(product(prop_size.checked_add(256).ok_or(MEMORY_ERROR)?, 2)?)?;
            tiles.entry(key).or_default().push(mvt::Feature { id: Some(feature.id), kind, geometry, props: props.clone() });
        }
        // Local geometry has moved into tiles; release only the now-dead property scratch.
        drop(props);
        budget.release(property_reserve)?;
    }
    if tiles.is_empty() {
        return Err("no geometry survives clipping and quantization at the requested zooms".into());
    }
    if notices.wrapped {
        bounds[0] = -180.0;
        bounds[2] = 180.0;
    }
    let metadata_size =
        dataset.fields.iter().try_fold(
            dataset.name.len().checked_add(source_layer.len()).and_then(|n| n.checked_add(1024)).ok_or(MEMORY_ERROR)?,
            |n, f| n.checked_add(f.name.len()).and_then(|n| n.checked_add(128)).ok_or(MEMORY_ERROR),
        )?;
    // Reserve before JSON cloning/escaping, including the writer's serialized copy.
    budget.add(product(metadata_size, 32)?)?;
    let fields: serde_json::Map<String, serde_json::Value> = dataset
        .fields
        .iter()
        .map(|f| {
            let ty = match f.ty {
                FieldType::Integer | FieldType::Double => "Number",
                // Vector tiles have no dates: they are ISO text.
                FieldType::Text | FieldType::Date | FieldType::DateTime => "String",
                FieldType::Boolean => "Boolean",
            };
            (f.name.clone(), serde_json::Value::String(ty.into()))
        })
        .collect();
    let metadata = serde_json::json!({
        "name": dataset.name,
        "format": "pbf",
        "type": "overlay",
        "minzoom": min_zoom,
        "maxzoom": max_zoom,
        "bounds": bounds,
        "vector_layers": [{"id": source_layer, "fields": fields, "minzoom": min_zoom, "maxzoom": max_zoom}]
    });
    let mut encoded = Vec::with_capacity(tiles.len());
    let mut encoded_size = 0usize;
    for ((z, x, y), features) in tiles {
        let tile = mvt::Tile { layers: vec![mvt::Layer { name: source_layer.into(), extent: EXTENT, features }] };
        let estimate = tile_size(&tile)?;
        // mvt::encode holds feature messages, values, dedup keys, layer and final bytes.
        budget.ensure(product(estimate, 16)?)?;
        let bytes = mvt::encode(&tile);
        encoded_size = encoded_size.checked_add(bytes.len()).ok_or(MEMORY_ERROR)?;
        budget.add(product(bytes.len().checked_add(128).ok_or(MEMORY_ERROR)?, 2)?)?;
        encoded.push((z, x, y, bytes));
    }
    // pmtiles::write retains input payloads while building deduplicated data and output.
    // Eight payload lengths cover growth and final copies; directories/hash/sort overhead
    // is reserved separately (far above the writer's per-entry representation).
    budget.ensure(product(encoded_size, 8)?.checked_add(product(encoded.len(), 2048)?).ok_or(MEMORY_ERROR)?)?;
    let tile_count = encoded.len();
    let bytes =
        pmtiles::write(encoded, &pmtiles::NewArchive { tile_type: pmtiles::TileType::Mvt, gzipped_tiles: false, bounds, metadata })?;
    let mut messages = vec![format!("Exported {tile_count} tiles at zooms {min_zoom}–{max_zoom}; clipping buffer is 8/4096 tile units.")];
    if notices.poles > 0 {
        messages.push(format!("Clamped {} coordinates to Web Mercator's ±{MAX_LAT}° latitude limit.", notices.poles));
    }
    if notices.collapsed > 0 {
        messages
            .push(format!("Dropped {} geometry parts collapsed or made topologically invalid by integer quantization.", notices.collapsed));
    }
    if notices.missing > 0 {
        messages.push(format!("Skipped {} features without geometry.", notices.missing));
    }
    Ok(Export { bytes, messages })
}

struct Budget {
    used: usize,
    work: usize,
}
impl Budget {
    fn ensure(&self, extra: usize) -> Result<(), String> {
        if self.used.checked_add(extra).is_none_or(|n| n > MAX_MEMORY) { Err(MEMORY_ERROR.into()) } else { Ok(()) }
    }
    fn add(&mut self, extra: usize) -> Result<(), String> {
        self.ensure(extra)?;
        self.used += extra;
        Ok(())
    }
    fn release(&mut self, bytes: usize) -> Result<(), String> {
        self.used = self.used.checked_sub(bytes).ok_or("vector export memory budget accounting underflow")?;
        Ok(())
    }
    fn add_work(&mut self, units: usize) -> Result<(), String> {
        let work = self.work.checked_add(units).ok_or(WORK_ERROR)?;
        if work > MAX_WORK {
            return Err(WORK_ERROR.into());
        }
        self.work = work;
        Ok(())
    }
    fn validate_polygon(&mut self, vertices: usize, rings: usize) -> Result<(), String> {
        // geo 0.33 validates segment pairs quadratically, including between rings.
        // One extra vertex per ring covers the implicit closure added by Polygon::new.
        let closed = vertices.checked_add(rings).ok_or(WORK_ERROR)?;
        self.add_work(closed.checked_mul(closed).ok_or(WORK_ERROR)?)
    }
}
fn product(a: usize, b: usize) -> Result<usize, String> {
    a.checked_mul(b).ok_or_else(|| MEMORY_ERROR.into())
}
fn geometry_memory(vertices: usize, rings: usize, polygon: bool) -> Result<usize, String> {
    let slots = vertices.checked_add(rings).and_then(|n| n.checked_add(8)).ok_or(MEMORY_ERROR)?;
    // Per vertex/ring, 256 bytes cover projected f64 coordinates, up to two clipped
    // endpoints per line segment, f32 quantization, Vec headers and capacity growth.
    // Polygons reserve 2048 instead: a valid ring can cross each of the rectangle's
    // four edges, introducing at most eight crossing endpoints per input segment.
    // This also covers geo's sweep/intersection buffers, disconnected output rings,
    // copied f64 rings for validation, and concurrent f32/f64 quantized copies.
    // The rectangle intersection is linear in size, not a vertex-by-vertex overlay.
    product(slots, if polygon { 2048 } else { 256 })
}

fn input_size(ds: &Dataset) -> Result<usize, String> {
    let mut b = Budget { used: size_of::<Dataset>(), work: 0 };
    b.add(ds.name.capacity())?;
    b.add(ds.source.as_ref().map_or(0, String::capacity))?;
    b.add(product(ds.features.capacity(), size_of::<rustgis_data::Feature>())?)?;
    b.add(product(ds.fields.capacity(), size_of::<rustgis_data::Field>())?)?;
    for field in &ds.fields {
        b.add(field.name.capacity())?;
        b.add(field.alias.as_ref().map_or(0, String::capacity))?;
    }
    for f in &ds.features {
        b.add(product(f.attrs.capacity(), size_of::<Value>())?)?;
        for v in &f.attrs {
            if let Value::Text(s) = v {
                b.add(s.capacity())?;
            }
        }
        let Some(g) = &f.geometry else { continue };
        let path = |p: &Vec<Coord>| product(p.capacity(), size_of::<Coord>());
        match g {
            Geometry::Point(_) => {}
            Geometry::MultiPoint(p) => b.add(path(p)?)?,
            Geometry::Line(parts) => {
                b.add(product(parts.capacity(), size_of::<Vec<Coord>>())?)?;
                for p in parts {
                    b.add(path(p)?)?;
                }
            }
            Geometry::Polygon(polys) => {
                b.add(product(polys.capacity(), size_of::<Vec<Vec<Coord>>>())?)?;
                for rings in polys {
                    b.add(product(rings.capacity(), size_of::<Vec<Coord>>())?)?;
                    for p in rings {
                        b.add(path(p)?)?;
                    }
                }
            }
        }
    }
    // Allocator bookkeeping, Arc storage and capacity slack must count too, not just payload.
    product(b.used, 2)
}

fn properties(ds: &Dataset, attrs: &[Value]) -> Result<Vec<(String, mvt::Value)>, String> {
    let mut out = Vec::new();
    for (field, value) in ds.fields.iter().zip(attrs) {
        let value = match value {
            Value::Null => continue,
            Value::Int(i) if i.unsigned_abs() > (1u64 << 53) => {
                return Err(format!("field '{}' contains an integer outside the exact ±2^53 range", field.name));
            }
            Value::Int(i) => mvt::Value::Number(*i as f64),
            Value::Float(f) if !f.is_finite() => return Err(format!("field '{}' contains a non-finite number", field.name)),
            Value::Float(f) => mvt::Value::Number(*f),
            Value::Text(s) => mvt::Value::String(s.clone()),
            Value::Bool(b) => mvt::Value::Bool(*b),
            Value::Date(_) | Value::DateTime(_) => mvt::Value::String(value.to_string()),
        };
        out.push((field.name.clone(), value));
    }
    Ok(out)
}
fn properties_size(props: &[(String, mvt::Value)]) -> Result<usize, String> {
    props.iter().try_fold(0usize, |n, (k, v)| {
        let text = if let mvt::Value::String(s) = v { s.len() } else { 16 };
        n.checked_add(k.len()).and_then(|n| n.checked_add(text)).and_then(|n| n.checked_add(128)).ok_or_else(|| MEMORY_ERROR.into())
    })
}
fn tile_size(tile: &mvt::Tile) -> Result<usize, String> {
    let mut n = 128usize;
    for layer in &tile.layers {
        n = n.checked_add(layer.name.len()).ok_or(MEMORY_ERROR)?;
        for f in &layer.features {
            n = n.checked_add(properties_size(&f.props)?).and_then(|n| n.checked_add(128)).ok_or(MEMORY_ERROR)?;
            for part in &f.geometry {
                n = n.checked_add(product(part.len(), 32)?).ok_or(MEMORY_ERROR)?;
            }
        }
    }
    Ok(n)
}

fn project(c: Coord, notices: &mut Notices, bounds: &mut [f64; 4]) -> Result<GC<f64>, String> {
    if !c.x.is_finite() || !c.y.is_finite() || c.y.abs() > 90.0 {
        return Err("geometry contains a non-finite coordinate or latitude outside ±90°".into());
    }
    let lon = (c.x.rem_euclid(360.0) + 180.0).rem_euclid(360.0) - 180.0;
    let lat = c.y.clamp(-MAX_LAT, MAX_LAT);
    if lat != c.y {
        notices.poles += 1;
    }
    bounds[0] = bounds[0].min(lon);
    bounds[1] = bounds[1].min(lat);
    bounds[2] = bounds[2].max(lon);
    bounds[3] = bounds[3].max(lat);
    let y = (1.0 - lat.to_radians().tan().asinh() / std::f64::consts::PI) * 0.5;
    Ok(GC { x: (lon + 180.0) / 360.0, y: y.clamp(0.0, 1.0) })
}
fn project_path(path: &[Coord], anchor: Option<f64>, notices: &mut Notices, bounds: &mut [f64; 4]) -> Result<Vec<GC<f64>>, String> {
    let mut out: Vec<GC<f64>> = Vec::with_capacity(path.len());
    for c in path {
        let mut p = project(*c, notices, bounds)?;
        if let Some(previous) = out.last().map(|p| p.x) {
            let shift = (previous - p.x).round();
            p.x += shift;
            notices.wrapped |= shift != 0.0;
        }
        out.push(p);
    }
    if let (Some(anchor), Some(centre)) = (anchor, path_centre(&out)) {
        let shift = (anchor - centre).round();
        for p in &mut out {
            p.x += shift;
        }
        notices.wrapped |= shift != 0.0;
    }
    if let (Some(first), Some(last)) = (out.first(), out.last())
        && path.first() == path.last()
        && (first.x - last.x).abs() > 0.5
    {
        return Err("a polygon winds around the whole world; split it at the antimeridian before export".into());
    }
    Ok(out)
}

fn path_centre(path: &[GC<f64>]) -> Option<f64> {
    let first = path.first()?;
    let (lo, hi) = path.iter().fold((first.x, first.x), |(lo, hi), p| (lo.min(p.x), hi.max(p.x)));
    Some((lo + hi) * 0.5)
}

fn emit(
    rings: &[Vec<GC<f64>>],
    kind: mvt::GeomKind,
    min: u8,
    max: u8,
    tiles: &mut BTreeMap<TileKey, Parts>,
    budget: &mut Budget,
    notices: &mut Notices,
) -> Result<(), String> {
    let mut bb = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in rings.iter().flatten() {
        bb[0] = bb[0].min(p.x);
        bb[1] = bb[1].min(p.y);
        bb[2] = bb[2].max(p.x);
        bb[3] = bb[3].max(p.y);
    }
    if !bb[0].is_finite() {
        notices.collapsed += 1;
        return Ok(());
    }
    if bb[2] - bb[0] > 1.0 + 1e-9 {
        return Err("geometry wraps more than one world; split it at the antimeridian before export".into());
    }
    let vertices = rings.iter().try_fold(0usize, |n, r| n.checked_add(r.len()).ok_or(MEMORY_ERROR))?;
    let scratch = geometry_memory(vertices, rings.len(), kind == mvt::GeomKind::Polygon)?;
    budget.ensure(scratch)?;
    // Preflight every requested zoom before clipping any candidates. Keep the ranges
    // on the stack; the public entry point allows at most seventeen zoom levels.
    let mut ranges = [[0i64; 4]; 17];
    for (range, z) in ranges.iter_mut().zip(min..=max) {
        let n = 1i64 << z;
        let scale = n as f64;
        let x0 = (bb[0] * scale - BUFFER).floor() as i64;
        let x1 = (bb[2] * scale + BUFFER).floor() as i64;
        let y0 = ((bb[1] * scale - BUFFER).floor() as i64).clamp(0, n - 1);
        let y1 = ((bb[3] * scale + BUFFER).floor() as i64).clamp(0, n - 1);
        let candidates = (x1 - x0 + 1).checked_mul(y1 - y0 + 1).ok_or(WORK_ERROR)?;
        if candidates > MAX_TILES as i64 * 4 {
            return Err("vector export candidate extent exceeds the 10,000 tile limit; reduce the zoom range or input extent".into());
        }
        let candidates = usize::try_from(candidates).map_err(|_| WORK_ERROR)?;
        budget.add_work(candidates.checked_mul(vertices).ok_or(WORK_ERROR)?)?;
        *range = [x0, x1, y0, y1];
    }
    let polygon = if kind == mvt::GeomKind::Polygon {
        let Some(polygon) = prepare_polygon(rings, budget, notices)? else { return Ok(()) };
        Some(polygon)
    } else {
        None
    };
    for ([x0, x1, y0, y1], z) in ranges.into_iter().zip(min..=max) {
        let n = 1i64 << z;
        let scale = n as f64;
        for x in x0..=x1 {
            for y in y0..=y1 {
                let rect = [
                    (x as f64 - BUFFER) / scale,
                    (y as f64 - BUFFER) / scale,
                    (x as f64 + 1.0 + BUFFER) / scale,
                    (y as f64 + 1.0 + BUFFER) / scale,
                ];
                budget.ensure(scratch)?;
                let parts = clip_prepared(rings, polygon.as_ref(), kind, rect, x, y, scale, budget, notices)?;
                if parts.is_empty() {
                    continue;
                }
                let key = (z, x.rem_euclid(n) as u32, y as u32);
                if !tiles.contains_key(&key) && tiles.len() >= MAX_TILES {
                    return Err("vector export exceeds the 10,000 tile limit; reduce the zoom range or input extent".into());
                }
                let coords = parts.iter().try_fold(0usize, |n, p| n.checked_add(p.len()).ok_or(MEMORY_ERROR))?;
                budget.add(
                    product(coords, 32)?.checked_add(product(parts.len(), 128)?).and_then(|n| n.checked_add(256)).ok_or(MEMORY_ERROR)?,
                )?;
                tiles.entry(key).or_default().extend(parts);
            }
        }
    }
    Ok(())
}

fn prepare_polygon(rings: &[Vec<GC<f64>>], budget: &mut Budget, notices: &mut Notices) -> Result<Option<Polygon<f64>>, String> {
    let Some(outer) = rings.first() else { return Ok(None) };
    if outer.len() < 3 {
        notices.collapsed += 1;
        return Ok(None);
    }
    let vertices = rings.iter().try_fold(0usize, |n, ring| n.checked_add(ring.len()).ok_or(WORK_ERROR))?;
    budget.validate_polygon(vertices, rings.len())?;
    let holes = rings
        .iter()
        .skip(1)
        .filter_map(|r| {
            if r.len() < 3 {
                notices.collapsed += 1;
                None
            } else {
                Some(LineString::new(r.clone()))
            }
        })
        .collect();
    let polygon = Polygon::new(LineString::new(outer.clone()), holes);
    if !polygon.is_valid() {
        return Err("polygon is topologically invalid after antimeridian unwrapping; repair its rings before export".into());
    }
    Ok(Some(polygon))
}

#[cfg(test)]
fn clipped(
    rings: &[Vec<GC<f64>>],
    kind: mvt::GeomKind,
    rect: [f64; 4],
    x: i64,
    y: i64,
    n: f64,
    notices: &mut Notices,
) -> Result<Parts, String> {
    let mut budget = Budget { used: 0, work: 0 };
    let polygon = if kind == mvt::GeomKind::Polygon { prepare_polygon(rings, &mut budget, notices)? } else { None };
    clip_prepared(rings, polygon.as_ref(), kind, rect, x, y, n, &mut budget, notices)
}

fn clip_prepared(
    rings: &[Vec<GC<f64>>],
    polygon: Option<&Polygon<f64>>,
    kind: mvt::GeomKind,
    rect: [f64; 4],
    x: i64,
    y: i64,
    n: f64,
    budget: &mut Budget,
    notices: &mut Notices,
) -> Result<Parts, String> {
    let quantize =
        |c: GC<f64>| [((c.x * n - x as f64) * EXTENT as f64).round() as f32, ((c.y * n - y as f64) * EXTENT as f64).round() as f32];
    let mut out = Vec::new();
    match kind {
        mvt::GeomKind::Point => {
            for p in rings.iter().flatten() {
                if p.x >= rect[0] && p.x <= rect[2] && p.y >= rect[1] && p.y <= rect[3] {
                    out.push(vec![quantize(*p)]);
                }
            }
        }
        mvt::GeomKind::LineString => {
            for ring in rings {
                if ring.len() < 2 {
                    notices.collapsed += 1;
                    continue;
                }
                let mut part = Vec::new();
                for pair in ring.windows(2) {
                    if let Some((a, b)) = clip_segment(pair[0], pair[1], rect) {
                        let (a, b) = (quantize(a), quantize(b));
                        if part.last().is_some_and(|last| *last != a) {
                            finish_line(&mut part, &mut out, notices);
                        }
                        if part.last() != Some(&a) {
                            part.push(a);
                        }
                        if part.last() != Some(&b) {
                            part.push(b);
                        }
                    } else {
                        finish_line(&mut part, &mut out, notices);
                    }
                }
                finish_line(&mut part, &mut out, notices);
            }
        }
        mvt::GeomKind::Polygon => {
            let Some(polygon) = polygon else { return Ok(out) };
            let mask = Rect::new(GC { x: rect[0], y: rect[1] }, GC { x: rect[2], y: rect[3] }).to_polygon();
            for polygon in polygon.intersection(&mask).0 {
                let Some(exterior) = quantized_ring(polygon.exterior(), &quantize, false, notices) else { continue };
                let holes: Vec<Vec<[f32; 2]>> =
                    polygon.interiors().iter().filter_map(|r| quantized_ring(r, &quantize, true, notices)).collect();
                let vertices = holes.iter().try_fold(exterior.len(), |n, ring| n.checked_add(ring.len()).ok_or(WORK_ERROR))?;
                budget.validate_polygon(vertices, holes.len().checked_add(1).ok_or(WORK_ERROR)?)?;
                let to_line = |r: &Vec<[f32; 2]>| LineString::new(r.iter().map(|p| GC { x: p[0] as f64, y: p[1] as f64 }).collect());
                let quantized = Polygon::new(to_line(&exterior), holes.iter().map(to_line).collect());
                // Rounding can make a hole touch the exterior or collapse a narrow neck.
                // Never emit a topologically invalid polygon as if it were unchanged.
                if !quantized.is_valid() {
                    notices.collapsed += 1;
                    continue;
                }
                out.push(exterior);
                out.extend(holes);
            }
        }
        mvt::GeomKind::Unknown => return Err("unsupported geometry type".into()),
    }
    Ok(out)
}
fn finish_line(part: &mut Vec<[f32; 2]>, out: &mut Parts, notices: &mut Notices) {
    if part.len() >= 2 {
        out.push(std::mem::take(part));
    } else if !part.is_empty() {
        notices.collapsed += 1;
        part.clear();
    }
}
fn quantized_ring(
    ring: &LineString<f64>,
    quantize: &impl Fn(GC<f64>) -> [f32; 2],
    hole: bool,
    notices: &mut Notices,
) -> Option<Vec<[f32; 2]>> {
    let mut out: Vec<_> = ring.0.iter().copied().map(quantize).collect();
    out.dedup();
    if let (Some(first), Some(last)) = (out.first(), out.last())
        && first != last
    {
        out.push(*first);
    }
    let area: f64 = out.windows(2).map(|p| p[0][0] as f64 * p[1][1] as f64 - p[1][0] as f64 * p[0][1] as f64).sum();
    if out.len() < 4 || area.abs() < 1.0 {
        notices.collapsed += 1;
        return None;
    }
    if (area > 0.0) == hole {
        out.reverse();
    }
    Some(out)
}
fn clip_segment(a: GC<f64>, b: GC<f64>, r: [f64; 4]) -> Option<(GC<f64>, GC<f64>)> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for (p, q) in [(-dx, a.x - r[0]), (dx, r[2] - a.x), (-dy, a.y - r[1]), (dy, r[3] - a.y)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                lo = lo.max(t);
            } else {
                hi = hi.min(t);
            }
            if lo > hi {
                return None;
            }
        }
    }
    Some((GC { x: a.x + lo * dx, y: a.y + lo * dy }, GC { x: a.x + hi * dx, y: a.y + hi * dy }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::{Feature, Field, GeometryType};

    fn dataset(geometry: Geometry) -> Dataset {
        let mut ds = Dataset::new("Deterministic export", geometry.kind(), vec![]);
        ds.features.push(Feature { id: u64::MAX, geometry: Some(geometry), attrs: vec![] });
        ds
    }
    fn coords(points: &[(f64, f64)]) -> Vec<Coord> {
        points.iter().map(|&(x, y)| Coord::new(x, y)).collect()
    }
    fn projected(points: &[(f64, f64)]) -> Vec<GC<f64>> {
        points.iter().map(|&(x, y)| GC { x, y }).collect()
    }
    fn section(bytes: &[u8], at: usize) -> Result<&[u8], String> {
        let offset = u64::from_le_bytes(bytes.get(at..at + 8).ok_or("short header")?.try_into().map_err(|_| "offset")?);
        let length = u64::from_le_bytes(bytes.get(at + 8..at + 16).ok_or("short header")?.try_into().map_err(|_| "length")?);
        let offset = usize::try_from(offset).map_err(|_| "offset too large")?;
        let length = usize::try_from(length).map_err(|_| "length too large")?;
        bytes.get(offset..offset.checked_add(length).ok_or("length overflow")?).ok_or_else(|| "short section".into())
    }
    fn only_tile(bytes: &[u8]) -> Result<mvt::Tile, String> {
        // These tests export only zoom zero: the archive has exactly one tile.
        mvt::decode(section(bytes, 56)?)
    }
    fn geometry_tiles(g: Geometry, z: u8) -> Result<(BTreeMap<TileKey, Parts>, Notices), String> {
        let mut notices = Notices::default();
        let mut bounds = [180.0, MAX_LAT, -180.0, -MAX_LAT];
        let mut tiles = BTreeMap::new();
        let mut budget = Budget { used: 0, work: 0 };
        match g {
            Geometry::Line(parts) => {
                for p in parts {
                    let path = project_path(&p, None, &mut notices, &mut bounds)?;
                    emit(&[path], mvt::GeomKind::LineString, z, z, &mut tiles, &mut budget, &mut notices)?;
                }
            }
            Geometry::Polygon(polys) => {
                for rings in polys {
                    let mut paths = Vec::new();
                    let mut anchor = None;
                    for ring in rings {
                        let path = project_path(&ring, anchor, &mut notices, &mut bounds)?;
                        anchor = paths.first().and_then(|p: &Vec<GC<f64>>| p.first()).map(|p| p.x).or_else(|| path.first().map(|p| p.x));
                        paths.push(path);
                    }
                    emit(&paths, mvt::GeomKind::Polygon, z, z, &mut tiles, &mut budget, &mut notices)?;
                }
            }
            _ => return Err("test helper needs a path".into()),
        }
        Ok((tiles, notices))
    }

    #[test]
    fn archive_preserves_ids_types_nulls_and_metadata() -> Result<(), String> {
        let mut ds = dataset(Geometry::Point(Coord::new(20.0, 30.0)));
        ds.fields = vec![
            Field::new("name", FieldType::Text),
            Field::new("count", FieldType::Integer),
            Field::new("ratio", FieldType::Double),
            Field::new("active", FieldType::Boolean),
            Field::new("empty", FieldType::Text),
        ];
        ds.features[0].attrs = vec![Value::Text("a\"b".into()), Value::Int(1i64 << 53), Value::Float(1.25), Value::Bool(true), Value::Null];
        let a = export(&ds, &[], 0, 0, "source_id")?;
        let b = export(&ds, &[], 0, 0, "source_id")?;
        assert_eq!(a.bytes, b.bytes);
        assert_eq!(&a.bytes[..8], b"PMTiles\x03");
        assert_eq!(&a.bytes[97..102], &[1, 1, 1, 0, 0]);
        let tile = only_tile(&a.bytes)?;
        let layer = &tile.layers[0];
        assert_eq!(layer.name, "source_id");
        assert_eq!(layer.extent, EXTENT);
        let f = &layer.features[0];
        assert_eq!(f.id, Some(u64::MAX));
        assert_eq!(f.prop("name"), Some(&mvt::Value::String("a\"b".into())));
        assert_eq!(f.prop("count"), Some(&mvt::Value::Number((1u64 << 53) as f64)));
        assert_eq!(f.prop("ratio"), Some(&mvt::Value::Number(1.25)));
        assert_eq!(f.prop("active"), Some(&mvt::Value::Bool(true)));
        assert_eq!(f.prop("empty"), None);
        let meta: serde_json::Value = serde_json::from_slice(section(&a.bytes, 24)?).map_err(|e| e.to_string())?;
        assert_eq!(meta["vector_layers"][0]["id"], "source_id");
        assert_eq!(meta["vector_layers"][0]["fields"]["active"], "Boolean");
        assert_eq!(meta["bounds"], serde_json::json!([20.0, 30.0, 20.0, 30.0]));
        Ok(())
    }

    #[test]
    fn attributed_points_release_per_feature_property_scratch() -> Result<(), String> {
        let mut ds = Dataset::new("Attributed points", GeometryType::Point, vec![]);
        ds.fields = (0..10).map(|i| Field::new(format!("f{i}"), FieldType::Text)).collect();
        ds.features = (0..8_000)
            .map(|id| Feature {
                id,
                geometry: Some(Geometry::Point(Coord::new(20.0, 30.0))),
                attrs: (0..10).map(|i| Value::Text(format!("v{i}"))).collect(),
            })
            .collect();
        // Retained properties and encoder scratch fit, but retaining every feature's
        // temporary property reserve as well would exceed the memory limit.
        let archive = export(&ds, &[], 0, 0, "attributed")?;
        assert_eq!(&archive.bytes[..8], b"PMTiles\x03");
        assert!(archive.messages.iter().any(|message| message.starts_with("Exported 1 tiles ")));
        let tile = only_tile(&archive.bytes)?;
        assert_eq!(tile.layers.len(), 1);
        let layer = &tile.layers[0];
        assert_eq!(layer.name, "attributed");
        assert_eq!(layer.features.len(), ds.features.len());
        for (expected, actual) in ds.features.iter().zip(&layer.features) {
            assert_eq!(actual.id, Some(expected.id));
            assert_eq!(actual.kind, mvt::GeomKind::Point);
            assert_eq!(actual.geometry.len(), 1);
            assert_eq!(actual.geometry[0].len(), 1);
            assert_eq!(actual.props.len(), ds.fields.len());
            for (i, field) in ds.fields.iter().enumerate() {
                assert_eq!(actual.prop(&field.name), Some(&mvt::Value::String(format!("v{i}"))));
            }
        }
        Ok(())
    }

    #[test]
    fn invalid_polygon_reports_feature_id_without_returning_a_partial_archive() -> Result<(), String> {
        let mut ds =
            dataset(Geometry::Polygon(vec![vec![coords(&[(10.0, 10.0), (20.0, 10.0), (20.0, 20.0), (10.0, 20.0), (10.0, 10.0)])]]));
        ds.features[0].id = 7;
        let valid = export(&ds, &[], 0, 0, "polygons")?;
        assert_eq!(only_tile(&valid.bytes)?.layers[0].features[0].id, Some(7));
        let invalid_id = 98_765_432;
        ds.features.push(Feature {
            id: invalid_id,
            geometry: Some(Geometry::Polygon(vec![vec![coords(&[(30.0, 10.0), (40.0, 20.0), (30.0, 20.0), (40.0, 10.0), (30.0, 10.0)])]])),
            attrs: vec![],
        });
        let error = match export(&ds, &[], 0, 0, "polygons") {
            Ok(_) => return Err("invalid polygon returned an archive instead of rejecting the whole export".into()),
            Err(error) => error,
        };
        assert_eq!(
            error,
            format!("feature {invalid_id}: polygon is topologically invalid after antimeridian unwrapping; repair its rings before export")
        );
        Ok(())
    }

    #[test]
    fn rejects_unsafe_integer_and_nonfinite_attributes() {
        let mut ds = dataset(Geometry::Point(Coord::new(0.0, 0.0)));
        ds.fields.push(Field::new("n", FieldType::Integer));
        for value in [Value::Int((1i64 << 53) + 1), Value::Int(-((1i64 << 53) + 1)), Value::Int(i64::MIN), Value::Float(f64::NAN)] {
            ds.features[0].attrs = vec![value];
            assert!(export(&ds, &[], 0, 0, "test").is_err());
        }
    }

    #[test]
    fn holes_keep_winding_and_disconnected_clip_pieces() -> Result<(), String> {
        let rings = vec![
            projected(&[(0.1, 0.1), (0.9, 0.1), (0.9, 0.9), (0.1, 0.9), (0.1, 0.1)]),
            projected(&[(0.3, 0.3), (0.7, 0.3), (0.7, 0.7), (0.3, 0.7), (0.3, 0.3)]),
        ];
        let mut notices = Notices::default();
        let all = clipped(&rings, mvt::GeomKind::Polygon, [0.0, 0.0, 1.0, 1.0], 0, 0, 1.0, &mut notices)?;
        assert_eq!(all.len(), 2);
        assert!(mvt::ring_area(&all[0]) > 0.0);
        assert!(mvt::ring_area(&all[1]) < 0.0);
        // A horizontal cut through the donut produces two disconnected exteriors,
        // not one artificial connector or a free-standing hole.
        let split = clipped(&rings, mvt::GeomKind::Polygon, [0.0, 0.4, 1.0, 0.6], 0, 0, 1.0, &mut notices)?;
        assert_eq!(split.len(), 2);
        assert!(split.iter().all(|r| mvt::ring_area(r) > 0.0));
        Ok(())
    }

    #[test]
    fn antimeridian_line_uses_only_edge_columns() -> Result<(), String> {
        let (tiles, notices) = geometry_tiles(Geometry::Line(vec![coords(&[(179.0, 12.0), (-179.0, 12.0)])]), 4)?;
        assert!(notices.wrapped);
        assert_eq!(tiles.len(), 2);
        assert!(tiles.keys().all(|&(_, x, _)| x == 0 || x == 15));
        for parts in tiles.values() {
            assert!(parts.iter().flatten().all(|p| p[0] >= -8.0 && p[0] <= 4104.0));
        }
        Ok(())
    }

    #[test]
    fn antimeridian_polygon_and_hole_do_not_span_world() -> Result<(), String> {
        let g = Geometry::Polygon(vec![vec![
            coords(&[(178.0, 10.0), (-178.0, 10.0), (-178.0, 20.0), (178.0, 20.0), (178.0, 10.0)]),
            coords(&[(179.0, 12.0), (-179.0, 12.0), (-179.0, 18.0), (179.0, 18.0), (179.0, 12.0)]),
        ]]);
        let (tiles, notices) = geometry_tiles(g, 4)?;
        assert!(notices.wrapped);
        assert!(!tiles.is_empty());
        assert!(tiles.keys().all(|&(_, x, _)| x == 0 || x == 15));
        assert!(tiles.values().flatten().all(|r| mvt::ring_area(r) > 0.0));
        Ok(())
    }

    #[test]
    fn source_crs_is_provenance_and_output_is_web_mercator() -> Result<(), String> {
        // A reader has already normalized source coordinates to degrees; attaching its
        // source CRS must neither reinterpret nor transform these coordinates a second time.
        let normalized = coords(&[(15.0, 90.0), (15.0, -90.0), (15.0, 45.0)]);
        let mut ds = dataset(Geometry::MultiPoint(normalized.clone()));
        let a = export(&ds, &[], 0, 0, "polar")?;
        ds.crs = Some(rustgis_data::raster::Crs::WebMercator);
        let b = export(&ds, &[], 0, 0, "polar")?;
        assert_eq!(a.bytes, b.bytes);
        assert_eq!(ds.features[0].geometry, Some(Geometry::MultiPoint(normalized)));
        assert!(a.messages.iter().any(|m| m.contains("Clamped 2")));
        let tile = only_tile(&a.bytes)?;
        let p = &tile.layers[0].features[0].geometry;
        assert_eq!(p.len(), 3);
        assert!(p.iter().all(|part| part[0][0] == 2219.0));
        assert_eq!(p[0][0][1], 0.0);
        assert_eq!(p[1][0][1], 4096.0);
        // 45°N projects nonlinearly to y ~= 1473.434 in a 4096-unit zoom-zero tile.
        assert_eq!(p[2][0][1], 1473.0);
        Ok(())
    }

    #[test]
    fn boundary_points_are_buffered_into_adjacent_tiles() -> Result<(), String> {
        let mut tiles = BTreeMap::new();
        let mut notices = Notices::default();
        emit(&[projected(&[(0.5, 0.5)])], mvt::GeomKind::Point, 1, 1, &mut tiles, &mut Budget { used: 0, work: 0 }, &mut notices)?;
        assert_eq!(tiles.len(), 4);
        assert_eq!(tiles[&(1, 0, 0)][0][0], [4096.0, 4096.0]);
        assert_eq!(tiles[&(1, 1, 1)][0][0], [0.0, 0.0]);
        Ok(())
    }

    #[test]
    fn quantization_collapses_are_dropped_and_reported() -> Result<(), String> {
        let ds = dataset(Geometry::Line(vec![coords(&[(20.0, 20.0), (20.000_000_001, 20.0)]), coords(&[(20.0, 20.0), (21.0, 21.0)])]));
        let result = export(&ds, &[], 0, 0, "test")?;
        assert!(result.messages.iter().any(|m| m.contains("Dropped 1")));
        let tile = only_tile(&result.bytes)?;
        assert_eq!(tile.layers[0].features[0].geometry.len(), 1);
        let tiny = dataset(Geometry::Line(vec![coords(&[(20.0, 20.0), (20.000_000_001, 20.0)])]));
        assert!(export(&tiny, &[], 0, 0, "test").is_err());
        Ok(())
    }

    #[test]
    fn collapsed_hole_is_not_emitted_as_exterior() -> Result<(), String> {
        let rings = vec![
            projected(&[(0.1, 0.1), (0.9, 0.1), (0.9, 0.9), (0.1, 0.9), (0.1, 0.1)]),
            projected(&[(0.4, 0.4), (0.400_01, 0.4), (0.400_01, 0.400_01), (0.4, 0.400_01), (0.4, 0.4)]),
        ];
        let mut notices = Notices::default();
        let out = clipped(&rings, mvt::GeomKind::Polygon, [0.0, 0.0, 1.0, 1.0], 0, 0, 1.0, &mut notices)?;
        assert_eq!(out.len(), 1);
        assert!(mvt::ring_area(&out[0]) > 0.0);
        assert_eq!(notices.collapsed, 1);
        Ok(())
    }

    #[test]
    fn zoom_tile_and_memory_limits_are_checked() -> Result<(), String> {
        let ds = dataset(Geometry::Point(Coord::new(0.0, 0.0)));
        assert!(export(&ds, &[], 2, 1, "test").is_err());
        assert!(export(&ds, &[], 0, 17, "test").is_err());
        assert!(export(&ds, &[], 16, 16, "test").is_ok());
        let wide = dataset(Geometry::Line(vec![coords(&[(-80.0, -70.0), (80.0, 70.0)])]));
        let error = export(&wide, &[], 16, 16, "test").err().ok_or("expected tile limit")?;
        assert!(error.contains("10,000"));
        assert!(Budget { used: MAX_MEMORY, work: 0 }.ensure(1).is_err());
        assert!(Budget { used: 0, work: 0 }.ensure(usize::MAX).is_err());
        assert!(geometry_memory(usize::MAX, 1, true).is_err());
        assert!(geometry_memory(usize::MAX / 2048, 1, true).is_err());
        assert!(geometry_memory(usize::MAX / 256, 1, false).is_err());
        assert!(product(usize::MAX, 2).is_err());
        assert!(Budget { used: 0, work: 0 }.validate_polygon(usize::MAX, 1).is_err());
        assert!(Budget { used: 0, work: 0 }.validate_polygon(usize::MAX / 2, 0).is_err());
        assert!(Budget { used: 0, work: usize::MAX }.add_work(1).is_err());
        Ok(())
    }

    #[test]
    fn releasing_scratch_preserves_retained_memory_and_checks_underflow() -> Result<(), String> {
        let mut budget = Budget { used: 64, work: 0 };
        budget.add(128)?;
        budget.add(32)?;
        budget.release(128)?;
        assert_eq!(budget.used, 96);
        assert!(budget.release(97).is_err());
        assert_eq!(budget.used, 96);
        Ok(())
    }

    fn circular_ring(vertices: usize) -> Vec<Coord> {
        (0..vertices)
            .map(|i| {
                let angle = i as f64 * std::f64::consts::TAU / vertices as f64;
                Coord::new(20.0 + 20.0 * angle.cos(), 20.0 + 20.0 * angle.sin())
            })
            .chain(std::iter::once(Coord::new(40.0, 20.0)))
            .collect()
    }

    #[test]
    fn valid_polygon_with_over_two_thousand_vertices_exports_at_low_zoom() -> Result<(), String> {
        let ds = dataset(Geometry::Polygon(vec![vec![circular_ring(2400)]]));
        let archive = export(&ds, &[], 0, 0, "detailed")?;
        let tile = only_tile(&archive.bytes)?;
        let feature = &tile.layers[0].features[0];
        assert_eq!(feature.kind, mvt::GeomKind::Polygon);
        assert!(!feature.geometry.is_empty());
        assert!(feature.geometry.iter().all(|ring| mvt::ring_area(ring) > 0.0));
        assert!(!archive.messages.iter().any(|message| message.contains("Dropped")));
        Ok(())
    }

    #[test]
    fn realistic_line_with_over_sixty_five_thousand_vertices_exports() -> Result<(), String> {
        let points = (0..70_000)
            .map(|i| {
                let t = i as f64 / 69_999.0;
                Coord::new(20.0 + 0.2 * t, 20.0 + 0.01 * (t * std::f64::consts::TAU * 8.0).sin())
            })
            .collect();
        let ds = dataset(Geometry::Line(vec![points]));
        let archive = export(&ds, &[], 0, 0, "detailed")?;
        let tile = only_tile(&archive.bytes)?;
        let feature = &tile.layers[0].features[0];
        assert_eq!(feature.kind, mvt::GeomKind::LineString);
        assert!(feature.geometry.iter().any(|part| part.len() >= 2));
        Ok(())
    }

    #[test]
    fn quadratic_polygon_validation_has_a_separate_work_limit() -> Result<(), String> {
        let ds = dataset(Geometry::Polygon(vec![vec![circular_ring(10_000)]]));
        let error = export(&ds, &[], 0, 0, "detailed").err().ok_or("expected polygon work limit")?;
        assert_eq!(error, format!("feature {}: {WORK_ERROR}", u64::MAX));
        Ok(())
    }

    #[test]
    fn candidate_vertex_work_is_cumulative_across_zooms_before_clipping() -> Result<(), String> {
        let ring: Vec<_> = (0..5000)
            .map(|i| {
                let t = i as f64 / 4999.0;
                GC { x: 0.25 + 0.5 * t, y: 0.25 + 0.5 * t }
            })
            .collect();
        // Buffered rectangles have 66² and 130² candidates, respectively. Either
        // zoom fits alone, but their cumulative vertex scans exceed the work cap.
        assert!(66 * 66 * ring.len() < MAX_WORK);
        assert!(130 * 130 * ring.len() < MAX_WORK);
        assert!((66 * 66 + 130 * 130) * ring.len() > MAX_WORK);
        let mut tiles = BTreeMap::new();
        let error = emit(&[ring], mvt::GeomKind::LineString, 7, 8, &mut tiles, &mut Budget { used: 0, work: 0 }, &mut Notices::default())
            .err()
            .ok_or("expected cumulative candidate work limit")?;
        assert_eq!(error, WORK_ERROR);
        assert!(tiles.is_empty());
        Ok(())
    }

    #[test]
    fn quantized_polygon_validation_uses_the_same_cumulative_work_budget() -> Result<(), String> {
        let rings = vec![projected(&[(0.1, 0.1), (0.9, 0.1), (0.9, 0.9), (0.1, 0.9), (0.1, 0.1)])];
        let mut budget = Budget { used: 0, work: 0 };
        let mut notices = Notices::default();
        let polygon = prepare_polygon(&rings, &mut budget, &mut notices)?;
        budget.work = MAX_WORK - 1;
        let error =
            clip_prepared(&rings, polygon.as_ref(), mvt::GeomKind::Polygon, [0.0, 0.0, 1.0, 1.0], 0, 0, 1.0, &mut budget, &mut notices)
                .err()
                .ok_or("expected quantized validity work limit")?;
        assert_eq!(error, WORK_ERROR);
        Ok(())
    }

    #[test]
    fn selected_rows_multipart_and_invalid_coordinates() -> Result<(), String> {
        let mut ds = dataset(Geometry::MultiPoint(coords(&[(1.0, 2.0), (3.0, 4.0)])));
        ds.features.push(Feature { id: 42, geometry: Some(Geometry::Point(Coord::new(10.0, 20.0))), attrs: vec![] });
        let selected = export(&ds, &[1], 0, 0, "test")?;
        assert_eq!(only_tile(&selected.bytes)?.layers[0].features[0].id, Some(42));
        assert!(export(&ds, &[2], 0, 0, "test").is_err());
        assert!(export(&ds, &[], 0, 0, "").is_err());
        for c in [Coord::new(f64::NAN, 0.0), Coord::new(0.0, 91.0), Coord::new(0.0, f64::INFINITY)] {
            assert!(export(&dataset(Geometry::Point(c)), &[], 0, 0, "test").is_err());
        }
        assert!(export(&Dataset::new("empty", GeometryType::Point, vec![]), &[], 0, 0, "test").is_err());
        Ok(())
    }
}
