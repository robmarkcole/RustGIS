//! Satellite embeddings (Home › Add › Web Data › Satellite Embeddings): the catalog of embedding
//! datasets, a search for a dataset's tiles where the map is, and a tile shown on the map —
//! AlphaEarth's as a cloud-optimised GeoTIFF of three bands, Earth Index's as points coloured by
//! their vectors' principal components, Tessera's as a raster of three bands of the view.
//!
//! On the desktop and the command line these commands read what they need, waiting for it; the
//! web app reads it off the frame and hands it over (`ran`).

use std::collections::BTreeMap;

use rustgis_data::raster;
use rustgis_map::raster::RasterLayer;
use serde_json::{Value, json};

use super::{f, ps};
use crate::render::rustgis_tiles::embeddings::{self as emb, tessera};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("embeddings.datasets", "Satellite Embedding Datasets", "Home › Add › Web Data", |_, _| {
            Ok(json!({"datasets": emb::DATASETS.iter().map(|d| d.to_json()).collect::<Vec<_>>()}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        // Not a view: it adds the footprints layer (by default).
        C::new("embeddings.search", "Search Satellite Embeddings", "Home › Add › Web Data", search).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"dataset": "alphaearth" | "earth-index" | "tessera", "year"?: number (AlphaEarth and Tessera: 2017–2025, default 2024), "bbox"?: [west, south, east, north] | "view" (the map's extent, the default), "footprints"?: bool (adds the tiles' footprints as a layer, default true)}"#,
        ),
        C::new("embeddings.add", "Add Satellite Embeddings", "Home › Add › Web Data", add).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"dataset": "alphaearth" | "earth-index" | "tessera", "url"?: "AlphaEarth's tile or Earth Index's file, as embeddings.search lists it (Earth Index: default the MGRS tile at the view's centre)", "year"?: number (Tessera, default 2024), "bands"?: [red, green, blue] (bands from 1; AlphaEarth default [2, 17, 10], Tessera [1, 2, 3]), "stretch"?: number (AlphaEarth: dequantised values shown over ±stretch, default 0.3), "dimension"?: number (Earth Index: a vector value kept as each point's `value`, from 1), "bbox"?: [west, south, east, north] | "view" (Earth Index and Tessera: the area read, default the map's extent), "name"?: text, "ran"?: (internal: what the app read)}"#,
        ),
        C::new("embeddings.addCube", "Add Satellite Embeddings over Time", "Home › Add › Web Data", add_cube).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network)).params(
            r#"{"dataset": "alphaearth", "bbox"?: [west, south, east, north] | "view" (the area searched, default the map's extent), "start"?: year (default 2017), "end"?: year (default 2025), "bands"?: [red, green, blue] (bands from 1, default [2, 17, 10]), "stretch"?: number (dequantised values shown over ±stretch, default 0.3), "name"?: text, "found"?: (internal: the tiles the app found, each {years: {year: address}}), "complete"?: (internal: false when the app's search read only part of the index)} → a tile layer per AlphaEarth tile, a file per year, following the time slider (turned on over the years when it is off)"#,
        ),
    ]
}

/// The box a command names, the map's extent by default.
fn bbox(s: &Session, p: &Value) -> Result<[f64; 4], String> {
    match p.get("bbox") {
        Some(Value::Array(a)) => {
            let n: Option<Vec<f64>> = a.iter().map(Value::as_f64).collect();
            match n.as_deref().unwrap_or_default() {
                [w, south, e, n] => emb::check_bbox([*w, *south, *e, *n]),
                _ => Err("bbox is [west, south, east, north], or \"view\"".into()),
            }
        }
        Some(Value::String(v)) if v != "view" => Err("bbox is [west, south, east, north], or \"view\"".into()),
        _ => {
            let e = super::workspace::view_extent(s);
            if e.is_empty() {
                return Err("the map's extent isn't known; give bbox".into());
            }
            emb::check_bbox([e.min_x, e.min_y, e.max_x, e.max_y])
        }
    }
}

/// The searchable dataset a command names.
fn dataset(p: &Value) -> Result<&'static emb::EmbeddingDataset, String> {
    let id = ps(p, "dataset").map(str::trim).ok_or("dataset is required (alphaearth, earth-index or tessera)")?;
    let d = emb::dataset(id).ok_or_else(|| format!("unknown dataset '{id}' (embeddings.datasets lists them)"))?;
    if !d.search {
        return Err(format!("{} is listed for its links only: RustGIS doesn't read it", d.name));
    }
    Ok(d)
}

/// The year a command names, within the dataset's.
fn year(d: &emb::EmbeddingDataset, p: &Value) -> Result<i32, String> {
    let (first, last) = d.years.unwrap_or((2024, 2024));
    let y = match f(p, "year") {
        Some(y) if y.fract() == 0.0 && y >= f64::from(first) && y <= f64::from(last) => y as i32,
        Some(_) => return Err(format!("{} has the years {first}–{last}", d.name)),
        None => 2024.clamp(first, last),
    };
    Ok(y)
}

/// Three bands from 1 a command names (0-based), each below `max`.
fn bands(p: &Value, max: usize, default: [usize; 3]) -> Result<[usize; 3], String> {
    match p.get("bands") {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Array(a)) => {
            let b: Option<Vec<usize>> =
                a.iter().map(|v| v.as_u64().filter(|b| (1..=max as u64).contains(b)).map(|b| b as usize - 1)).collect();
            match b.as_deref() {
                Some([r, g, b]) => Ok([*r, *g, *b]),
                _ => Err(format!("bands is [red, green, blue], each from 1 to {max}")),
            }
        }
        Some(_) => Err(format!("bands is [red, green, blue], each from 1 to {max}")),
    }
}

/// A band list's label, from 1.
fn band_label(b: [usize; 3]) -> String {
    b.iter().map(|b| (b + 1).to_string()).collect::<Vec<_>>().join(", ")
}

fn search(s: &mut Session, p: &Value) -> CmdResult {
    let d = dataset(p)?;
    let year = year(d, p)?;
    let area = bbox(s, p)?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (year, area);
        Err("in the web app, search with Home › Add › Web Data › Satellite Embeddings".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (tiles, complete) = match d.id {
            "alphaearth" => emb::search_aef_blocking(area, year)?,
            "earth-index" => emb::search_earth_index_blocking(area)?,
            _ => tessera::tiles(area, year),
        };
        let mut r = json!({"dataset": d.id, "year": year, "count": tiles.len(), "complete": complete,
            "tiles": tiles.iter().map(emb::Tile::to_json).collect::<Vec<_>>()});
        if !complete {
            s.notify(format!("{}: the area holds more tiles than one search reads; zoom in to list them all", d.name), false);
        }
        if super::b(p, "footprints") != Some(false) && !tiles.is_empty() && d.id != "tessera" {
            let name = format!("{} tiles {year}", d.name.split(" (").next().unwrap_or(d.name));
            if let Some(id) = super::stac::add_collection(s, &emb::footprints_geojson(&tiles), &name)? {
                r["layer"] = json!(id);
            }
        }
        Ok(r)
    }
}

fn add(s: &mut Session, p: &Value) -> CmdResult {
    let d = dataset(p)?;
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
    match d.id {
        "alphaearth" => {
            let url = ps(p, "url")
                .map(str::trim)
                .filter(|u| u.starts_with("https://"))
                .ok_or("url is required: an AlphaEarth tile (embeddings.search lists them)")?;
            let b = bands(p, 64, emb::AEF_DEFAULT_BANDS)?;
            let look = emb::aef_look(b, stretch(p)?);
            let file = crate::render::rustgis_tiles::cog::split(url).0;
            // The year from the tile's address (…/annual/2024/10N/…), when it has one.
            let year = file.split('/').find(|s| s.len() == 4 && s.parse::<i32>().is_ok());
            let name = name.unwrap_or_else(|| {
                let title = year.map(|y| format!("AlphaEarth {y}")).unwrap_or_else(|| "AlphaEarth".into());
                format!("{title} ({})", band_label(b))
            });
            // Its header read first, so its bands are known; the layer is framed only when it is the
            // map's first content (no "zoom" is passed, so add_cog defaults it to that). In the
            // browser nothing may wait, nor may a plugin's command in the app (it runs on the
            // frame): it is read as the map draws.
            let probe = cfg!(not(target_arch = "wasm32")) && !s.leave_downloads;
            #[cfg(not(target_arch = "wasm32"))]
            if probe && !crate::render::rustgis_tiles::cog::probe_blocking(file)? {
                return Err(format!("{file}: not a cloud-optimised GeoTIFF"));
            }
            let look = crate::render::rustgis_tiles::cog::with_look(file, &look);
            super::layer::add_cog(s, &json!({"name": name}), &look, probe)
        }
        "earth-index" => add_points(s, p, name),
        _ => add_tessera(s, p, d, name),
    }
}

/// AlphaEarth's stretch a command names: dequantised values shown over ±stretch. Only a stretch
/// left out (or null) takes the default: one given that isn't a number is refused.
fn stretch(p: &Value) -> Result<f32, String> {
    let stretch = match p.get("stretch").filter(|v| !v.is_null()) {
        None => emb::AEF_DEFAULT_STRETCH,
        Some(_) => f(p, "stretch").map_or(f32::NAN, |v| v as f32),
    };
    if !(stretch.is_finite() && stretch > 0.0 && stretch <= 1.0) {
        return Err("stretch is a number above 0, at most 1".into());
    }
    Ok(stretch)
}

/// `embeddings.addCube` (#1720): AlphaEarth's tiles where the map is as a data cube, a year a step
/// of the time slider. Each tile is a tile layer reading its file of the year the slider is at
/// (`time_urls`, as `stac.addCube` makes them); a year a tile has no file for shows nothing, said.
/// The tiles come from one read of the index, which holds every year (the command line waits for
/// it), or from the app's search off the frame (`found`). Without `found`, natively, only the
/// first tile's latest year's header is read before the layers are added, as `embeddings.add`
/// reads one tile's; with `found` (the pane, on the frame) no header is read. The rest are read in
/// place as the map draws them.
///
/// Without `found` the command runs on its caller's thread and waits there for the index and that
/// header, as `embeddings.search` and `embeddings.add` do, so that the caller finds the layers
/// there when the command returns. The command line and Python have no window to hold. The app's
/// own routes (ribbon, command search) open the pane, which searches off the frame and hands its
/// tiles over (`found`); a plugin's command in the app (`leave_downloads`) is refused rather than
/// wait on the frame. The app's assistant runs its tools on the frame without `leave_downloads`
/// (see [`Session::run_leaving_downloads`]), so there it waits on the frame, as it does for every
/// command that waits for the network (`embeddings.search`, `layer.addUrl`); moving the
/// assistant's tools off the frame is a change for all of them, not for this one.
fn add_cube(s: &mut Session, p: &Value) -> CmdResult {
    use crate::render::rustgis_tiles::cog::{self, Look};
    let d = dataset(p)?;
    if d.id != "alphaearth" {
        return Err(format!("{}: only AlphaEarth's years make a time cube here", d.name));
    }
    let (first, last) = d.years.unwrap_or((2017, 2025));
    // Only a year left out (or null) takes the default: one given that isn't a year is refused.
    let year_param = |k: &str, default: i32| match p.get(k).filter(|v| !v.is_null()) {
        None => Ok(default),
        Some(_) => match f(p, k) {
            Some(y) if y.fract() == 0.0 && y >= f64::from(first) && y <= f64::from(last) => Ok(y as i32),
            _ => Err(format!("{k} is a year from {first} to {last}")),
        },
    };
    let (start, end) = (year_param("start", first)?, year_param("end", last)?);
    if start > end {
        return Err("start is a year before end, or the same one".into());
    }
    let b = bands(p, 64, emb::AEF_DEFAULT_BANDS)?;
    let look = Look { cube: true, ..emb::aef_look(b, stretch(p)?) };
    let given = p.get("found").filter(|v| !v.is_null());
    // Whether the index was read whole: a tile-year with no file is then the index's gap, not
    // the read's. The files handed over outside the years asked for, left out. And what the
    // search left out, said once the cube is added (a refused one adds nothing to speak of).
    let (series, complete, outside, notes) = match given {
        Some(v) => {
            let (series, outside) = found(v, start, end)?;
            let complete = super::b(p, "complete") != Some(false);
            // A part read: said even when every year of the tiles handed over is there.
            let notes = if complete { Vec::new() } else { vec![PART_READ.to_string()] };
            (series, complete, outside, notes)
        }
        None => {
            let (series, complete, notes) = search_series(s, p, (start, end))?;
            (series, complete, 0, notes)
        }
    };
    let listed = series.len();
    let series: Vec<BTreeMap<i32, String>> = series.into_iter().filter(|y| !y.is_empty()).collect();
    if series.is_empty() {
        return Err(format!("AlphaEarth has no tiles there for {start}–{end}"));
    }
    let room = rustgis_map::tiles::MAX_TILE_LAYERS.saturating_sub(s.map().tiles.len());
    if series.len() > room {
        return Err(format!(
            "{} AlphaEarth tiles, and the map has room for {room} more tile layers (at most {}): take a smaller area",
            series.len(),
            rustgis_map::tiles::MAX_TILE_LAYERS
        ));
    }
    let base = ps(p, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map_or_else(|| format!("AlphaEarth {start}–{end} ({})", band_label(b)), str::to_string);
    // A cube that can't be added whole takes out what it added (as stac.addCube does).
    let (tiles_before, layers_before): (Vec<rustgis_map::TileLayer>, Vec<u32>) =
        (s.map().tiles.clone(), s.map().layers.iter().map(|l| l.id).collect());
    let undo = |s: &mut Session| {
        let m = s.map_mut();
        m.tiles = tiles_before.clone();
        m.layers.retain(|l| layers_before.contains(&l.id));
    };
    // In the browser nothing may wait, nor may the app (it hands its search over) or a plugin's
    // command in it (on the frame).
    let mut probe = cfg!(not(target_arch = "wasm32")) && !s.leave_downloads && given.is_none();
    let span = usize::try_from(end - start + 1).unwrap_or(1);
    // A tile's place goes in its layer's name when the cube has several, or when a tile layer
    // already has the name (the pane adds one tile at a time).
    let tagged = series.len() > 1 || s.map().tiles.iter().any(|t| t.name == base);
    let (mut dates, mut missing) = (Vec::new(), 0usize);
    for years in &series {
        missing += span.saturating_sub(years.len());
        let urls: Vec<(f64, String)> = years.iter().map(|(y, u)| (emb::year_start_ms(*y), cog::with_look(u, &look))).collect();
        let Some((_, latest)) = urls.last() else { continue };
        let file = cog::split(latest).0.to_string();
        #[cfg(not(target_arch = "wasm32"))]
        if probe {
            match cog::probe_blocking(&file) {
                Ok(true) => {}
                Ok(false) => {
                    undo(s);
                    return Err(format!("{file}: not a cloud-optimised GeoTIFF"));
                }
                Err(e) => {
                    undo(s);
                    return Err(e);
                }
            }
        }
        let name = match (tagged, emb::aef_place(&file)) {
            (true, Some(place)) => format!("{base} · {place}"),
            _ => base.clone(),
        };
        let r = match super::layer::add_cog(s, &json!({"name": name}), latest, probe) {
            Ok(r) => r,
            Err(e) => {
                undo(s);
                return Err(e);
            }
        };
        probe = false;
        let at = r.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok());
        match at.and_then(|i| s.map_mut().tiles.get_mut(i)).filter(|t| cog::split(&t.url).0 == file) {
            Some(t) => t.time_urls = urls.clone(),
            None => {
                undo(s);
                return Err(format!("{file}: not added as a tile layer that follows the time slider"));
            }
        }
        dates.extend(urls.iter().map(|(d, _)| *d));
    }
    if let Err(e) = super::stac::cube_slider(s, &mut dates, &json!({"unit": "year", "format": "YYYY"})) {
        undo(s);
        return Err(e);
    }
    for note in notes {
        s.notify(note, false);
    }
    if outside > 0 {
        s.notify(
            if outside == 1 {
                format!("{base}: 1 file handed over is outside {start}–{end} and was left out")
            } else {
                format!("{base}: {outside} files handed over are outside {start}–{end} and were left out")
            },
            false,
        );
    }
    if listed > series.len() {
        s.notify(format!("{base}: {} of the {listed} tiles have no file in {start}–{end} and were left out", listed - series.len()), false);
    }
    if missing > 0 {
        let total = series.len() * span;
        s.notify(
            if complete {
                format!("{base}: {missing} of the {total} tile-years have no file in AlphaEarth's index; in those years the tile shows nothing")
            } else {
                format!(
                    "{base}: {missing} of the {total} tile-years have no file in the part of AlphaEarth's index that was read (zoom in to read it whole); in those years the tile shows nothing"
                )
            },
            false,
        );
    }
    Ok(json!({"tiles": series.len(), "years": dates.len(), "missing": missing}))
}

/// The tiles the app found and hands over (`found`): each a year's file by year, kept to the
/// years asked for; and how many files were outside them (said by the caller).
fn found(v: &Value, start: i32, end: i32) -> Result<(Vec<BTreeMap<i32, String>>, usize), String> {
    let bad = || "found is a list of tiles, each {\"years\": {\"2017\": an https address, …}}".to_string();
    let list = v.as_array().ok_or_else(bad)?;
    if list.len() > emb::MAX_TILES {
        return Err(format!("found holds {} tiles, more than the {} a search lists", list.len(), emb::MAX_TILES));
    }
    let mut outside = 0usize;
    let series = list
        .iter()
        .map(|t| {
            let (mut out, mut seen) = (BTreeMap::new(), std::collections::BTreeSet::new());
            for (y, u) in t.get("years").and_then(Value::as_object).ok_or_else(bad)? {
                let y: i32 = y.trim().parse().map_err(|_| bad())?;
                // Two keys of one year (" 2017" and "2017"): one file would be lost, so refused.
                if !seen.insert(y) {
                    return Err(format!("found: a tile names the year {y} twice"));
                }
                let u = u.as_str().map(str::trim).filter(|u| u.starts_with("https://") && u.len() <= 4000).ok_or_else(bad)?;
                if (start..=end).contains(&y) {
                    out.insert(y, u.to_string());
                } else {
                    outside += 1;
                }
            }
            Ok(out)
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((series, outside))
}

/// AlphaEarth's tiles over the years where the command says, from one read of the index, waiting
/// for it (the command line); whether it read the index whole for the area; and what the read
/// left out, to be said.
fn search_series(s: &mut Session, p: &Value, years: (i32, i32)) -> Result<(Vec<BTreeMap<i32, String>>, bool, Vec<String>), String> {
    let area = bbox(s, p)?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (area, years);
        Err("in the web app, add AlphaEarth's years with Home › Add › Web Data › Satellite Embeddings".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // A plugin's command in the app runs on the frame, where nothing may wait for the index.
        if s.leave_downloads {
            return Err(
                "AlphaEarth's index is read off the frame here: add its years from Home › Add › Web Data › Satellite Embeddings".into()
            );
        }
        let f = emb::search_aef_series_blocking(area, years)?;
        let notes = series_notes(&f);
        Ok((f.series.into_iter().map(|x| x.years).collect(), f.complete, notes))
    }
}

/// A search over the years that read part of the index, said.
const PART_READ: &str = "AlphaEarth: the area holds more tiles than one search reads; zoom in to take them all";

/// What a search over the years left out: rows past one read, tiles past a search's list, and a
/// tile's second file for a year.
#[cfg(not(target_arch = "wasm32"))]
fn series_notes(f: &emb::SeriesFound) -> Vec<String> {
    let mut notes = Vec::new();
    if !f.complete {
        notes.push(PART_READ.to_string());
    }
    if f.dropped > 0 {
        notes.push(format!("AlphaEarth: {} tiles past the {} a search lists were left out; zoom in for them", f.dropped, emb::MAX_TILES));
    }
    if f.duplicates > 0 {
        notes.push(format!("AlphaEarth: {} tile-years had a second file in the index; the first was taken", f.duplicates));
    }
    notes
}

/// Bytes the app read and handed over, by their id (`ran`).
fn handed(s: &mut Session, p: &Value) -> Result<Option<(u64, Vec<u8>)>, String> {
    let Some(id) = f(p, "ran").filter(|v| *v >= 0.0 && v.fract() == 0.0).map(|v| v as u64) else { return Ok(None) };
    let i = s.downloaded.iter().position(|(d, _)| *d == id).ok_or("no such read (it was dropped when another project opened)")?;
    Ok(Some((id, s.downloaded.remove(i).1)))
}

/// Earth Index's points in the view, coloured by their principal components.
fn add_points(s: &mut Session, p: &Value, name: Option<String>) -> CmdResult {
    let ds = match handed(s, p)? {
        // The app's read: the points' GeoJSON.
        Some((_, bytes)) => {
            let read = std::str::from_utf8(&bytes)
                .map_err(|_| "the points aren't UTF-8 text".to_string())
                .and_then(|text| rustgis_data::io::read_geojson(text, name.as_deref().unwrap_or("Earth Index")).map_err(|e| e.to_string()));
            // A read handed over is used once, added or not (the pane reads again), so a failed
            // one doesn't stay in the session.
            read?.0
        }
        None => {
            let area = bbox(s, p)?;
            let url = match ps(p, "url").map(str::trim).filter(|u| !u.is_empty()) {
                Some(u) if u.starts_with("https://") => u.to_string(),
                Some(_) => return Err("url is an Earth Index file's web address".into()),
                None => {
                    let tile =
                        emb::mgrs_tile((area[0] + area[2]) / 2.0, (area[1] + area[3]) / 2.0).ok_or("the map's centre has no MGRS tile")?;
                    emb::earth_index_url(&tile)
                }
            };
            #[cfg(target_arch = "wasm32")]
            {
                let _ = url;
                return Err("in the web app, add Earth Index with Home › Add › Web Data › Satellite Embeddings".into());
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let v = emb::load_points_blocking(&url, area)?;
                if v.points.is_empty() {
                    return Err("Earth Index has no points there".into());
                }
                let dim = f(p, "dimension").filter(|d| *d >= 1.0 && d.fract() == 0.0).map(|d| d as usize - 1);
                let tile = url.rsplit('/').next().and_then(|f| f.split('_').next()).unwrap_or("");
                emb::points_dataset(name.as_deref().unwrap_or(&format!("Earth Index {tile}")), &v, dim)
            }
        }
    };
    let n = ds.features.len();
    let id = super::layer::add_dataset(s, ds);
    style_points(s, id);
    Ok(json!({"layer": id, "features": n}))
}

/// Colours a points layer by its `color` field (its principal components, 64 colours), as small
/// squares.
pub(crate) fn style_points(s: &mut Session, id: u32) {
    use rustgis_map::{Marker, Renderer, Rgba, Symbol};
    let Some(l) = s.map_mut().layer_mut(id) else { return };
    let Some(fi) = l.dataset.field_index("color") else { return };
    let mut colors: Vec<String> = l.dataset.features.iter().filter_map(|f| f.attrs.get(fi)).map(|v| v.to_string()).collect();
    colors.sort();
    colors.dedup();
    let symbol = |c: Rgba| Symbol { fill: c, stroke: c, stroke_width: 0.0, size: 5.0, marker: Marker::Square };
    let classes: Vec<(String, Symbol)> =
        colors.into_iter().take(256).filter_map(|c| Rgba::from_hex(&c).map(|rgba| (c, symbol(rgba)))).collect();
    l.renderer = Renderer::Unique {
        field: "color".into(),
        classes,
        other: symbol(Rgba::rgb(128, 128, 128)),
        ramp: Default::default(),
        class_labels: Default::default(),
        other_label: None,
        class_strokes: Default::default(),
        other_strokes: Vec::new(),
        class_fills: Default::default(),
        other_fills: Vec::new(),
        expression: None,
    };
}

/// Tessera's three bands of the view, as a raster.
fn add_tessera(s: &mut Session, p: &Value, d: &emb::EmbeddingDataset, name: Option<String>) -> CmdResult {
    let year = year(d, p)?;
    let b = bands(p, tessera::BANDS, tessera::DEFAULT_BANDS)?;
    let (r, tif) = match handed(s, p)? {
        Some((_, tif)) => match raster::read_bytes("tessera.tif", &tif) {
            Ok(r) if r.bands.iter().any(|band| band.stats.count > 0) => (r, tif),
            Ok(_) => return Err("Tessera has no embeddings there".into()),
            Err(e) => return Err(e.to_string()),
        },
        None => {
            let area = bbox(s, p)?;
            #[cfg(target_arch = "wasm32")]
            {
                let _ = area;
                return Err("in the web app, add Tessera with Home › Add › Web Data › Satellite Embeddings".into());
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let (r, cut) = tessera::read_blocking(area, year, b)?;
                if cut {
                    s.notify("Tessera: the view is larger than one read; its centre was read (zoom in for the rest)", false);
                }
                let tif = raster::geotiff::write(&r, 0).map_err(|e| e.to_string())?;
                (r, tif)
            }
        }
    };
    if r.bands.iter().all(|band| band.stats.count == 0) {
        return Err("Tessera has no embeddings there".into());
    }
    let name = name.unwrap_or_else(|| format!("Tessera {year} ({})", band_label(b)));
    let layer = RasterLayer::from_bytes(r, format!("tessera_{year}.tif"), tif);
    super::raster::add_raster(s, &name, layer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Project, Session};

    #[test]
    fn parameters() {
        let mut s = Session::new(Project::default());
        assert_eq!(s.run("embeddings.datasets", &json!({})).unwrap()["datasets"].as_array().map(Vec::len), Some(6));
        for bad in [
            json!({}),
            json!({"dataset": "clay"}),
            json!({"dataset": "nope"}),
            json!({"dataset": "alphaearth", "year": 1999, "bbox": [0, 0, 1, 1]}),
            json!({"dataset": "alphaearth", "bbox": [1, 0, 0, 1]}),
            json!({"dataset": "alphaearth", "bbox": "here"}),
        ] {
            assert!(s.run("embeddings.search", &bad).is_err(), "{bad}");
        }
        for bad in [
            json!({"dataset": "alphaearth"}),
            json!({"dataset": "alphaearth", "url": "file:///etc/passwd"}),
            json!({"dataset": "alphaearth", "url": "https://x.org/a.tiff", "bands": [0, 1, 2]}),
            json!({"dataset": "alphaearth", "url": "https://x.org/a.tiff", "bands": [1, 2]}),
            json!({"dataset": "alphaearth", "url": "https://x.org/a.tiff", "stretch": -1}),
            json!({"dataset": "earth-index", "url": "ftp://x"}),
            json!({"dataset": "tessera", "bands": [1, 2, 200]}),
            json!({"dataset": "tessera", "ran": 99}),
            json!({"dataset": "earth-index", "ran": 98}),
        ] {
            assert!(s.run("embeddings.add", &bad).is_err(), "{bad}");
        }
        assert_eq!(bands(&json!({"bands": [2, 17, 10]}), 64, [0, 0, 0]), Ok([1, 16, 9]));
        assert_eq!(band_label([1, 16, 9]), "2, 17, 10");
        let tile = json!([{"years": {"2017": "https://x.org/2017/23S/a-1-2.tiff"}}]);
        for bad in [
            json!({"dataset": "tessera", "found": tile}),
            json!({"dataset": "clay", "found": tile}),
            json!({"dataset": "alphaearth", "start": 2016, "found": tile}),
            json!({"dataset": "alphaearth", "end": 2024.5, "found": tile}),
            json!({"dataset": "alphaearth", "start": 2024, "end": 2020, "found": tile}),
            json!({"dataset": "alphaearth", "stretch": 2, "found": tile}),
            // A stretch that isn't a number, not taken as the default.
            json!({"dataset": "alphaearth", "stretch": "big", "found": tile}),
            json!({"dataset": "alphaearth", "stretch": true, "found": tile}),
            json!({"dataset": "alphaearth", "bands": [1, 2], "found": tile}),
            json!({"dataset": "alphaearth", "found": "tiles"}),
            json!({"dataset": "alphaearth", "found": [{"years": {"2017": "file:///etc/passwd"}}]}),
            json!({"dataset": "alphaearth", "found": [{"years": {"later": "https://x.org/a.tiff"}}]}),
            json!({"dataset": "alphaearth", "found": [{"files": []}]}),
            // A year that isn't one, not taken as the default.
            json!({"dataset": "alphaearth", "start": "2020-01-01", "found": tile}),
            json!({"dataset": "alphaearth", "end": true, "found": tile}),
            // A year named twice, one file of it lost.
            json!({"dataset": "alphaearth", "found": [{"years": {"2017": "https://x.org/a.tiff", " 2017": "https://x.org/b.tiff"}}]}),
            // Nothing in the years asked for.
            json!({"dataset": "alphaearth", "start": 2020, "found": tile}),
            json!({"dataset": "alphaearth", "found": vec![json!({"years": {}}); emb::MAX_TILES + 1]}),
        ] {
            assert!(s.run("embeddings.addCube", &bad).is_err(), "{bad}");
        }
        assert!(s.map().tiles.is_empty() && s.map().time.is_none(), "a refused cube leaves the map as it was");
    }

    /// The tiles the app found make a cube: a tile layer each, a file a year in the slider's order,
    /// the slider on over the years; the years a tile has no file for are said.
    #[test]
    fn alphaearth_years_on_the_time_slider() {
        let mut s = Session::new(Project::default());
        let file = |y: i32, place: &str| format!("https://127.0.0.1:9/annual/{y}/23S/x{y}-{place}.tiff");
        let (a, b) = ("0000008192-0000000000", "0000000000-0000000000");
        let found = json!([
            {"years": {"2025": file(2025, a), "2017": file(2017, a), "2016": file(2016, a)}},
            {"years": {"2020": file(2020, b)}},
        ]);
        let r = s.run("embeddings.addCube", &json!({"dataset": "alphaearth", "found": found, "bands": [1, 2, 3]})).unwrap();
        assert_eq!((r["tiles"].as_u64(), r["years"].as_u64(), r["missing"].as_u64()), (Some(2), Some(3), Some(15)));
        assert_eq!(s.map().tiles.len(), 2);
        let named = |s: &Session, place: &str| s.map().tiles.iter().find(|t| t.name.ends_with(place)).cloned().unwrap();
        let first = named(&s, a);
        assert_eq!(first.name, format!("AlphaEarth 2017–2025 (1, 2, 3) · 23S {a}"));
        let dates: Vec<f64> = first.time_urls.iter().map(|(d, _)| *d).collect();
        assert_eq!(dates, vec![emb::year_start_ms(2017), emb::year_start_ms(2025)], "2016 is outside the years asked for");
        let (f, look) = crate::render::rustgis_tiles::cog::split(&first.time_urls[0].1);
        assert_eq!(f, file(2017, a));
        assert!(look.aef && look.cube && look.bands == vec![0, 1, 2], "{look:?}");
        let t = s.map().time.clone().unwrap();
        assert_eq!(t.dates, vec![emb::year_start_ms(2017), emb::year_start_ms(2020), emb::year_start_ms(2025)]);
        assert_eq!(t.format, "YYYY");
        assert!(s.notices.iter().any(|n| n.text.contains("15 of the 18 tile-years have no file")));
        assert!(
            s.notices.iter().any(|n| n.text.contains("1 file handed over is outside 2017–2025 and was left out")),
            "a file outside the years asked for is said: {:?}",
            s.notices.iter().map(|n| &n.text).collect::<Vec<_>>()
        );
        // The slider shows each tile's file of its year, or nothing in a year it has none for.
        s.run("time.goTo", &json!({"step": 1})).unwrap();
        assert!(named(&s, a).time_hidden, "no file in 2020");
        let other = named(&s, b);
        assert!(!other.time_hidden && other.url.starts_with(&file(2020, b)), "{}", other.url);
        // Every year there: nothing said.
        let mut t = Session::new(Project::default());
        let one = json!([{"years": {"2024": file(2024, "1-2"), "2025": file(2025, "1-2")}}]);
        t.run("embeddings.addCube", &json!({"dataset": "alphaearth", "start": 2024, "found": one, "name": "AEF"})).unwrap();
        assert_eq!(t.map().tiles.first().map(|l| l.name.as_str()), Some("AEF"));
        assert!(!t.notices.iter().any(|n| n.text.contains("tile-years")));
        // The pane adds a tile at a time: a second cube of the same name takes its place in it.
        let mut w = Session::new(Project::default());
        for place in [a, b] {
            let one = json!([{"years": {"2024": file(2024, place)}}]);
            w.run("embeddings.addCube", &json!({"dataset": "alphaearth", "found": one})).unwrap();
        }
        let names: Vec<&str> = w.map().tiles.iter().map(|l| l.name.as_str()).collect();
        assert!(names.len() == 2 && names[0] != names[1] && names.iter().any(|n| n.ends_with(b)), "{names:?}");
        // A search that read part of the index: the missing years are the read's, said so; a tile
        // with no file in the years asked for is left out, said.
        let mut u = Session::new(Project::default());
        let part = json!([{"years": {"2025": file(2025, "3-4")}}, {"years": {"2016": file(2016, "5-6")}}]);
        let r = u.run("embeddings.addCube", &json!({"dataset": "alphaearth", "start": 2024, "found": part, "complete": false})).unwrap();
        assert_eq!((r["tiles"].as_u64(), r["missing"].as_u64()), (Some(1), Some(1)));
        assert!(
            u.notices.iter().any(|n| n.text.contains("1 of the 2 tile-years have no file in the part of AlphaEarth's index that was read"))
        );
        assert!(u.notices.iter().any(|n| n.text.contains("1 of the 2 tiles have no file in 2024–2025 and were left out")));
        // A part read with every year there: still said.
        let mut v = Session::new(Project::default());
        let whole = json!([{"years": {"2024": file(2024, "7-8"), "2025": file(2025, "7-8")}}]);
        v.run("embeddings.addCube", &json!({"dataset": "alphaearth", "start": 2024, "found": whole, "complete": false})).unwrap();
        assert!(v.notices.iter().any(|n| n.text.contains("more tiles than one search reads")), "a part read is said");
        // An hourly slider widened over the years would be more steps than it lists: refused,
        // the slider and the map as they were.
        let mut h = Session::new(Project::default());
        h.run("time.set", &json!({"start": "2024-06-01", "end": "2024-06-02", "unit": "hour"})).unwrap();
        let before = h.map().time.clone();
        let two = json!([{"years": {"2017": file(2017, a), "2025": file(2025, a)}}]);
        let e = h.run("embeddings.addCube", &json!({"dataset": "alphaearth", "found": two})).unwrap_err();
        assert!(e.contains("steps"), "{e}");
        assert!(h.map().tiles.is_empty() && h.map().time == before, "a refused cube leaves the map as it was");
        // A slider stepping two years at a time would show 2018's file in 2017's step, and never
        // 2017's: refused, saying which dates; a yearly one takes the cube.
        let nine: serde_json::Map<String, Value> = (2017..=2025).map(|y| (y.to_string(), json!(file(y, a)))).collect();
        let nine = json!([{"years": nine}]);
        let mut g = Session::new(Project::default());
        g.run("time.set", &json!({"start": "2017", "end": "2025", "unit": "year", "interval": 2})).unwrap();
        let before = g.map().time.clone();
        let e = g.run("embeddings.addCube", &json!({"dataset": "alphaearth", "found": nine})).unwrap_err();
        assert!(e.contains("2017-01-01") && e.contains("step"), "{e}");
        assert!(g.map().tiles.is_empty() && g.map().time == before);
        let mut y = Session::new(Project::default());
        y.run("time.set", &json!({"start": "2017", "end": "2025", "unit": "year"})).unwrap();
        y.run("embeddings.addCube", &json!({"dataset": "alphaearth", "found": nine})).unwrap();
        // Other bands (Apply to layers) hold at every year, not only the one shown.
        t.run("map.setTiles", &json!({"index": 0, "bands": [3, 4, 5]})).unwrap();
        t.run("time.goTo", &json!({"step": 0})).unwrap();
        let tile = t.map().tiles.first().cloned().unwrap();
        assert!(tile.url.starts_with(&file(2024, "1-2")), "{}", tile.url);
        for (_, u) in tile.time_urls.iter().chain([&(0.0, tile.url.clone())]) {
            let look = crate::render::rustgis_tiles::cog::split(u).1;
            assert!(look.bands == vec![2, 3, 4] && look.aef && look.cube, "{u}");
        }
    }

    /// What a search over the years left out is said, each with its count; a whole search says
    /// nothing.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn series_notes_say_what_a_search_left_out() {
        let whole = emb::SeriesFound { complete: true, ..Default::default() };
        assert!(series_notes(&whole).is_empty(), "a whole search says nothing");
        let gaps = emb::SeriesFound { complete: false, dropped: 61, duplicates: 2, ..Default::default() };
        let notes = series_notes(&gaps);
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(notes.iter().any(|n| n == PART_READ), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("61 tiles past the 60 a search lists were left out")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("2 tile-years had a second file in the index")), "{notes:?}");
    }

    /// A plugin's command in the app (`leave_downloads`) doesn't read an AlphaEarth tile's header
    /// on the frame: the layer is added and read as the map draws.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn alphaearth_added_without_waiting_when_downloads_are_left() {
        let url = "https://127.0.0.1:9/annual/2024/10N/x.tiff";
        let mut s = Session::new(Project::default());
        s.leave_downloads = true;
        let r = s.run("embeddings.add", &json!({"dataset": "alphaearth", "url": url}));
        assert!(r.is_ok(), "{r:?}");
        // The tile is added with its AlphaEarth look.
        let tile = s.map().tiles.last().map(|t| t.url.clone()).unwrap_or_default();
        let (file, look) = crate::render::rustgis_tiles::cog::split(&tile);
        assert_eq!(file, url);
        assert!(look.aef, "{tile}");
        // Elsewhere its header is read first, so an unreadable tile is refused.
        let mut t = Session::new(Project::default());
        assert!(t.run("embeddings.add", &json!({"dataset": "alphaearth", "url": url})).is_err());
    }

    #[test]
    fn points_coloured_by_their_components() {
        let mut s = Session::new(Project::default());
        let v = rustgis_data::source::parquet::Vectors {
            points: vec![(1.0, 2.0), (1.1, 2.0), (1.2, 2.1)],
            dims: 2,
            values: vec![0.0, 1.0, 5.0, 3.0, 9.0, 0.0],
        };
        let ds = emb::points_dataset("ei", &v, None);
        let text = rustgis_data::io::to_geojson(&ds).to_string();
        let id = s.download_started();
        assert!(s.download_landed(id, text.into_bytes()));
        let r = s.run("embeddings.add", &json!({"dataset": "earth-index", "ran": id, "name": "EI"})).unwrap();
        assert_eq!(r["features"], 3);
        let l = s.map().layers.iter().find(|l| l.dataset.name == "EI").unwrap();
        assert!(matches!(&l.renderer, rustgis_map::Renderer::Unique { field, classes, .. } if field == "color" && !classes.is_empty()));
        // Bytes that can't be added are dropped, not kept in the session.
        let id = s.download_started();
        assert!(s.download_landed(id, vec![0xff, 0xfe]));
        assert!(s.run("embeddings.add", &json!({"dataset": "earth-index", "ran": id})).is_err());
        assert!(!s.downloaded.iter().any(|(d, _)| *d == id));
    }
}
