//! First reads only the dates that can fill a cell, and firstDate gives the date each cell's
//! value came from (`cube.reduce`, #3182): over a small STAC collection served here, and a stack
//! of layers.
use super::*;

/// `size` × `size` cells over [10, 11] × [45, 46], in degrees, as `values` (row by row) say.
fn cog(values: Vec<f32>, size: u32) -> Vec<u8> {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let px = 1.0 / f64::from(size);
    let t = GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: px, pixel_height: -px };
    let r = Raster::new("r", size, size, vec![values], t, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
    rustgis_data::raster::cogwrite::write(&r, 0).unwrap()
}

/// The four dates, A to D.
const DATES: [&str; 4] = ["2024-01-01", "2024-01-17", "2024-02-02", "2024-02-18"];

/// Date k's red value at cell (col, row): 100 (k + 1) plus the cell's place.
fn red(k: usize, col: usize, row: usize) -> f32 {
    (100 * (k + 1) + row * 8 + col) as f32
}

/// Four dates of a red band over 8 × 8 cells and their SCL-like masks (4 clear, 9 cloud), date k
/// clear at (col, row) where `clear(k, col, row)` says: the API's address and the files'.
fn collection(clear: fn(usize, usize, usize) -> bool) -> (String, String) {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for k in 0..4 {
        files.push((format!("/b{k}.tif"), cog((0..64).map(|c| red(k, c % 8, c / 8)).collect(), 8)));
        files.push((format!("/m{k}.tif"), cog((0..64).map(|c| if clear(k, c % 8, c / 8) { 4.0 } else { 9.0 }).collect(), 8)));
    }
    let (files, _) = serve_ranged_files(files);
    let features: Vec<String> = DATES
        .iter()
        .enumerate()
        .map(|(k, d)| {
            format!(
                r#"{{"type": "Feature", "id": "s{k}", "collection": "s2", "bbox": [10, 45, 11, 46], "geometry": null,
                   "properties": {{"datetime": "{d}T00:00:00Z", "bdc:tiles": ["t1"]}},
                   "assets": {{"B04": {{"href": "{files}/b{k}.tif"}}, "SCL": {{"href": "{files}/m{k}.tif"}}}}}}"#
            )
        })
        .collect();
    let search = format!(r#"{{"type": "FeatureCollection", "features": [{}]}}"#, features.join(","));
    (format!("{}/v1", serve_routes(vec![("/v1/search", search)], 64)), files)
}

/// Date `k`'s day, as firstDate gives it.
fn day(k: usize) -> f32 {
    (rustgis_data::time::parse_text(DATES[k]).unwrap() / 86_400_000.0) as f32
}

/// The first raster `cube.reduce` adds for `p`.
fn reduce(s: &mut Session, p: &Value) -> std::sync::Arc<rustgis_data::raster::Raster> {
    let r = s.run("cube.reduce", p).unwrap_or_else(|e| panic!("{p}: {e}"));
    let l = s.map().layer(r["layers"][0].as_u64().unwrap() as u32).unwrap();
    l.raster.as_ref().unwrap().data.clone().unwrap()
}

/// The band files of `files`' collection read during `run`, by their date (`b0` …), each once.
fn bands_read<T>(files: &str, run: impl FnOnce() -> T) -> (T, Vec<String>) {
    use crate::cmd::stac::BANDS_READ;
    // The separator too: one server's address can begin another's (port 4000, 40001).
    let files = format!("{files}/");
    let mine = |all: &[String]| all.iter().filter(|h| h.starts_with(&files)).count();
    let before = mine(&BANDS_READ.lock().unwrap());
    let out = run();
    let all = BANDS_READ.lock().unwrap();
    let mut read: Vec<String> =
        all.iter().filter(|h| h.starts_with(&files)).skip(before).map(|h| h.trim_start_matches(&files).to_string()).collect();
    read.sort();
    read.dedup();
    (out, read)
}

/// First in clear order reads date A's band, which fills every cell, and none of the others'
/// (clear only where A is); with a hole only date C fills, A's and C's. The same rasters as with
/// nothing left out, and firstDate gives each cell's date.
#[test]
fn first_reads_only_the_dates_that_fill_a_cell() {
    use crate::cmd::{cube::SKIPS, stac::READERS};
    let mut s = Session::new(Project::default());
    // One date at a time, so which are read doesn't hang on how the readers race.
    READERS.with(|r| r.set(Some(1)));
    let params = |api: &str, stat: &str| {
        json!({"url": api, "collection": "s2", "asset": "B04", "bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-02-29",
            "mask": {"asset": "SCL", "keep": [4]}, "stat": stat, "order": "clear"})
    };
    // A clear everywhere; B in the west half, C in the north half, D in the north-west quarter.
    let (api, files) = collection(|k, col, row| match k {
        0 => true,
        1 => col < 4,
        2 => row < 4,
        _ => col < 4 && row < 4,
    });
    let (first, read) = bands_read(&files, || reduce(&mut s, &params(&api, "first")));
    assert_eq!(read, ["b0.tif"]);
    for (c, v) in first.bands[0].data.iter().enumerate() {
        assert_eq!(*v, red(0, c % 8, c / 8), "cell {c}");
    }
    SKIPS.with(|o| o.set(false));
    let (all, read) = bands_read(&files, || reduce(&mut s, &params(&api, "first")));
    SKIPS.with(|o| o.set(true));
    assert_eq!(read, ["b0.tif", "b1.tif", "b2.tif", "b3.tif"], "nothing left out: every band read");
    assert_eq!(first.bands[0].data, all.bands[0].data);
    // A clear but at (5, 5); B in the west half; C at (5, 5) and in the two west columns; D in
    // those columns: ranked A, B, C, D.
    let (api, files) = collection(|k, col, row| match k {
        0 => (col, row) != (5, 5),
        1 => col < 4,
        2 => (col, row) == (5, 5) || col < 2,
        _ => col < 2,
    });
    let want = |c: usize| if c == 5 * 8 + 5 { 2 } else { 0 };
    let (first, read) = bands_read(&files, || reduce(&mut s, &params(&api, "first")));
    assert_eq!(read, ["b0.tif", "b2.tif"]);
    let (dates, read) = bands_read(&files, || reduce(&mut s, &params(&api, "firstDate")));
    assert_eq!(read, ["b0.tif", "b2.tif"]);
    for c in 0..64 {
        assert_eq!(first.bands[0].data[c], red(want(c), c % 8, c / 8), "first, cell {c}");
        assert_eq!(dates.bands[0].data[c], day(want(c)), "firstDate, cell {c}");
    }
    // The dates' legend: the two dates held, named.
    let named: Vec<(usize, &str)> =
        dates.bands[0].classes.iter().enumerate().filter(|(_, n)| !n.is_empty()).map(|(v, n)| (v, n.as_str())).collect();
    assert_eq!(named, [(day(0) as usize, DATES[0]), (day(2) as usize, DATES[2])]);
    // Several readers, the dates coming as read: the same rasters.
    READERS.with(|r| r.set(None));
    assert_eq!(reduce(&mut s, &params(&api, "first")).bands[0].data, first.bands[0].data);
    assert_eq!(reduce(&mut s, &params(&api, "firstDate")).bands[0].data, dates.bands[0].data);
    // minValid asking for two dates a cell counts the dates read: nothing is left out.
    let mut two = params(&api, "first");
    two["minValid"] = json!(0.5);
    READERS.with(|r| r.set(Some(1)));
    let (_, read) = bands_read(&files, || reduce(&mut s, &two));
    READERS.with(|r| r.set(None));
    assert_eq!(read.len(), 4, "{read:?}");
}

/// A later date first leaves out whole still counts the pixels its item's mask file doesn't
/// cover and says them, as a read would: date A, clear everywhere, fills every cell; B's mask
/// covers only the west half, so B is read for its east half's 32 pixels, and C, wholly masked,
/// isn't. The raster is A's.
#[test]
fn first_leaving_a_date_out_still_says_its_pixels_outside_their_mask() {
    use crate::cmd::stac::READERS;
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    READERS.with(|r| r.set(Some(1)));
    let west = {
        let t = GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: 0.125, pixel_height: -0.125 };
        let r = Raster::new("m", 4, 8, vec![vec![4.0; 32]], t, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        rustgis_data::raster::cogwrite::write(&r, 0).unwrap()
    };
    let mut files: Vec<(String, Vec<u8>)> =
        (0..3).map(|k| (format!("/b{k}.tif"), cog((0..64).map(|c| red(k, c % 8, c / 8)).collect(), 8))).collect();
    files.push(("/m0.tif".into(), cog(vec![4.0; 64], 8)));
    files.push(("/m1.tif".into(), west));
    files.push(("/m2.tif".into(), cog(vec![4.0; 64], 8)));
    let (files, _) = serve_ranged_files(files);
    let features: Vec<String> = DATES[..3]
        .iter()
        .enumerate()
        .map(|(k, d)| {
            format!(
                r#"{{"type": "Feature", "id": "s{k}", "collection": "s2", "bbox": [10, 45, 11, 46], "geometry": null,
                   "properties": {{"datetime": "{d}T00:00:00Z", "bdc:tiles": ["t1"]}},
                   "assets": {{"B04": {{"href": "{files}/b{k}.tif"}}, "SCL": {{"href": "{files}/m{k}.tif"}}}}}}"#
            )
        })
        .collect();
    let search = format!(r#"{{"type": "FeatureCollection", "features": [{}]}}"#, features.join(","));
    let api = format!("{}/v1", serve_routes(vec![("/v1/search", search)], 16));
    let mut s = Session::new(Project::default());
    for stat in ["first", "firstDate"] {
        let mark = s.notice_mark();
        let p = json!({"url": api, "collection": "s2", "asset": "B04", "bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-02-29",
            "mask": {"asset": "SCL", "keep": [4]}, "stat": stat, "order": "date"});
        let (r, read) = bands_read(&files, || reduce(&mut s, &p));
        for (c, v) in r.bands[0].data.iter().enumerate() {
            let want = if stat == "first" { red(0, c % 8, c / 8) } else { day(0) };
            assert_eq!(*v, want, "{stat} cell {c}");
        }
        assert_eq!(read, ["b0.tif", "b1.tif"], "{stat}");
        let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert_eq!(
            said.iter().filter(|t| t.contains("32 band pixel(s) with a value lie outside") && t.contains("/m1.tif")).count(),
            1,
            "{stat}: {said:?}"
        );
    }
    READERS.with(|r| r.set(None));
}

/// FirstDate over a stack: the date of the value first takes, by year one layer each with an
/// empty cell where no date has a value, a legend of the dates, and what it can't be asked.
#[test]
fn first_date_of_a_stack() {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    let mut s = Session::new(Project::default());
    let mut names = Vec::new();
    // Four layers of 2 × 1 cells: the first cell has a value only at the second date of 2020.
    let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 1.0, pixel_height: -1.0 };
    let dates = ["2020-01-01", "2020-02-01", "2020-03-01", "2021-05-01"];
    for (date, a, b) in [(dates[0], f32::NAN, 10.0), (dates[1], 7.0, 20.0), (dates[2], f32::NAN, 40.0), (dates[3], f32::NAN, 50.0)] {
        let name = format!("ndvi_{date}");
        let r = Raster::new(&name, 2, 1, vec![vec![a, b]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let bytes = geotiff::write(&r, 0).unwrap();
        crate::cmd::raster::add_raster(&mut s, &name, rustgis_map::RasterLayer::from_bytes(r, format!("{name}.tif"), bytes)).unwrap();
        names.push(json!(name));
    }
    let day = |d: &str| (rustgis_data::time::parse_text(d).unwrap() / 86_400_000.0) as f32;
    let layers = |s: &mut Session, p: Value| -> Vec<u32> {
        let r = s.run("cube.reduce", &p).unwrap_or_else(|e| panic!("{p}: {e}"));
        r["layers"].as_array().unwrap().iter().map(|l| l.as_u64().unwrap() as u32).collect()
    };
    let data = |s: &Session, id: u32| s.map().layer(id).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    let first = layers(&mut s, json!({"layers": names, "stat": "first", "order": "date"}));
    let at = layers(&mut s, json!({"layers": names, "stat": "firstDate", "order": "date"}));
    assert_eq!(data(&s, first[0]).bands[0].data, [7.0, 10.0]);
    assert_eq!(data(&s, at[0]).bands[0].data, [day(dates[1]), day(dates[0])]);
    // Latest first.
    let at = layers(&mut s, json!({"layers": names, "stat": "firstDate", "order": "-date"}));
    assert_eq!(data(&s, at[0]).bands[0].data, [day(dates[1]), day(dates[3])]);
    // By year: 2021's first cell has no date with a value.
    let by = layers(&mut s, json!({"layers": names, "stat": "firstDate", "by": "year"}));
    assert_eq!(by.len(), 2);
    let (y2020, y2021) = (data(&s, by[0]), data(&s, by[1]));
    assert_eq!(y2020.bands[0].data, [day(dates[1]), day(dates[0])]);
    assert!(y2021.bands[0].data[0].is_nan(), "{:?}", y2021.bands[0].data);
    assert_eq!(y2021.bands[0].data[1], day(dates[3]));
    // Each layer's legend names the dates it holds, a colour each.
    let l = s.map().layer(by[0]).unwrap().raster.as_ref().unwrap();
    let legend: Vec<String> = l.legend_classes(false).unwrap().into_iter().map(|(_, label)| label).collect();
    assert_eq!(legend, [dates[0], dates[1]]);
    assert!(s.map().layer(by[0]).unwrap().name.contains("firstDate 2020"), "{}", s.map().layer(by[0]).unwrap().name);
    // Only cube.reduce's, and order is its as first's.
    let e = s.run("zarr.reduce", &json!({"layer": names[0], "stat": "firstDate"})).unwrap_err();
    assert!(e.contains("cube.reduce's"), "{e}");
    let e = s.run("cube.reduce", &json!({"layers": names, "stat": "mean", "order": "date"})).unwrap_err();
    assert!(e.contains("firstDate's"), "{e}");
}

/// Stack layers of two cells, a date each, added to `s`: their names.
fn two_cell_stack(s: &mut Session, dates: &[(String, f32, f32)]) -> Vec<Value> {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 1.0, pixel_height: -1.0 };
    dates
        .iter()
        .map(|(date, a, b)| {
            let name = format!("ndvi_{date}");
            let r = Raster::new(&name, 2, 1, vec![vec![*a, *b]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
            let bytes = geotiff::write(&r, 0).unwrap();
            crate::cmd::raster::add_raster(s, &name, rustgis_map::RasterLayer::from_bytes(r, format!("{name}.tif"), bytes)).unwrap();
            json!(name)
        })
        .collect()
}

/// A day before 1970 has no name, but its cells are drawn and its number is in the legend with
/// the named days.
#[test]
fn first_date_draws_a_day_before_1970() {
    let mut s = Session::new(Project::default());
    let names = two_cell_stack(&mut s, &[("1969-12-31".into(), 5.0, f32::NAN), ("1970-01-02".into(), f32::NAN, 6.0)]);
    let r = s.run("cube.reduce", &json!({"layers": names, "stat": "firstDate", "order": "date"})).unwrap();
    let l = s.map().layer(r["layers"][0].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap();
    assert_eq!(l.data.as_ref().unwrap().bands[0].data, [-1.0, 1.0]);
    let legend: Vec<String> = l.legend_classes(false).unwrap().into_iter().map(|(_, label)| label).collect();
    assert_eq!(legend, ["-1", "1970-01-02"]);
}

/// The dates' names a run's layers hold, indexed by day, stay within a budget however many
/// groups it makes: past it, a layer keeps its days unnamed, and that is said.
#[test]
fn first_date_names_stay_within_a_budget() {
    let mut s = Session::new(Project::default());
    // A date a year from 1990 to 2089: one layer a year, each naming every day up to its date.
    let dates: Vec<(String, f32, f32)> = (1990..2090).map(|y| (format!("{y}-06-01"), 1.0, 1.0)).collect();
    let names = two_cell_stack(&mut s, &dates);
    let mark = s.notice_mark();
    let r = s.run("cube.reduce", &json!({"layers": names, "stat": "firstDate", "by": "year"})).unwrap();
    let layers: Vec<u32> = r["layers"].as_array().unwrap().iter().map(|l| l.as_u64().unwrap() as u32).collect();
    assert_eq!(layers.len(), 100);
    let held = |s: &Session, id: u32| s.map().layer(id).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    let slots: usize = layers.iter().map(|id| held(&s, *id).bands[0].classes.len()).sum();
    assert!(slots <= 2_000_000, "{slots} names held");
    // The first years named, the last not; said.
    let first = held(&s, layers[0]);
    assert_eq!(first.bands[0].class_name(f64::from(first.bands[0].data[0])), Some("1990-06-01"));
    assert!(held(&s, layers[99]).bands[0].classes.is_empty());
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(said.iter().any(|t| t.contains("dates unnamed")), "{said:?}");
}
