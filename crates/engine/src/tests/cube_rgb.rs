//! Several bands of a STAC collection summarised together into one layer (`cube.reduce`'s
//! `"assets"`, part 2 of #2585): the same mask and the same dates for each cell's bands, the median
//! and the first clear date taken as one date's pixel.
use super::*;

/// `size` × `size` cells over [10, 11] × [45, 46], in degrees, as `values` (row by row) say.
fn cog(values: Vec<f32>, size: u32, nodata: Option<f64>) -> Vec<u8> {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let px = 1.0 / f64::from(size);
    let t = GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: px, pixel_height: -px };
    let r = Raster::new("r", size, size, vec![values], t, Crs::LonLat, nodata, ColorHint::Gray, "Float32").unwrap();
    rustgis_data::raster::cogwrite::write(&r, 0).unwrap()
}

/// Each date's red, green and blue: ranked by their sums 80 (date 2), 90 (date 0), 100 (date 1),
/// while each band alone ranks the dates otherwise, so a band-by-band median mixes dates.
const RGB: [[f32; 3]; 3] = [[10.0, 50.0, 30.0], [20.0, 10.0, 70.0], [30.0, 40.0, 10.0]];

/// Three dates of B04, B03 and B02 (and B05 at half their cells) with an SCL-like mask (4 clear,
/// 9 cloud): the first cloudy in column 0, the second clear everywhere, the third cloudy in
/// column 7. The second's B02 has no value at (3, 3). A fourth item has only B04 and a fifth no
/// B04: both are left out.
fn collection() -> String {
    let flat = |v: f32| cog(vec![v; 64], 8, None);
    let mask = |cloudy: Option<usize>| cog((0..64).map(|c| if Some(c % 8) == cloudy { 9.0 } else { 4.0 }).collect(), 8, None);
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for (k, rgb) in RGB.iter().enumerate() {
        for (j, key) in ["B04", "B03", "B02"].iter().enumerate() {
            let file = if (k, j) == (1, 2) {
                cog((0..64).map(|c| if c == 27 { -1.0 } else { rgb[j] }).collect(), 8, Some(-1.0))
            } else {
                flat(rgb[j])
            };
            files.push((format!("/{key}_{k}.tif"), file));
        }
        files.push((format!("/B05_{k}.tif"), cog(vec![1.0; 16], 4, None)));
        files.push((format!("/m{k}.tif"), mask([Some(0), None, Some(7)][k])));
    }
    files.push(("/B04_3.tif".into(), flat(99.0)));
    files.push(("/m3.tif".into(), mask(None)));
    files.push(("/B03_4.tif".into(), flat(99.0)));
    files.push(("/B02_4.tif".into(), flat(99.0)));
    files.push(("/m4.tif".into(), mask(None)));
    let (files, _) = serve_ranged_files(files);
    let features: Vec<String> = ["2024-01-01", "2024-01-17", "2024-02-02", "2024-02-18", "2024-02-20"]
        .iter()
        .enumerate()
        .map(|(k, d)| {
            let keys: &[&str] = match k {
                0..3 => &["B04", "B03", "B02", "B05"],
                3 => &["B04"],
                _ => &["B03", "B02"],
            };
            let mut assets: Vec<String> = keys.iter().map(|a| format!(r#""{a}": {{"href": "{files}/{a}_{k}.tif"}}"#)).collect();
            assets.push(format!(r#""SCL": {{"href": "{files}/m{k}.tif"}}"#));
            format!(
                r#"{{"type": "Feature", "id": "s{k}", "collection": "s2", "bbox": [10, 45, 11, 46], "geometry": null,
                   "properties": {{"datetime": "{d}T00:00:00Z", "bdc:tiles": ["t1"]}}, "assets": {{{}}}}}"#,
                assets.join(", ")
            )
        })
        .collect();
    let search = format!(r#"{{"type": "FeatureCollection", "features": [{}]}}"#, features.join(","));
    format!("{}/v1", serve_routes(vec![("/v1/search", search)], 64))
}

#[test]
fn several_bands_summarised_as_one_rgb_layer() {
    let api = collection();
    let mut s = Session::new(Project::default());
    let base = json!({"url": api, "collection": "s2", "assets": ["B04", "B03", "B02"], "bbox": [10, 45, 11, 46],
        "start": "2024-01-01", "end": "2024-02-29", "mask": {"asset": "SCL", "keep": [4]}});
    let with = |extra: Value| {
        let mut p = base.clone();
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    };
    // The layer's bands, and what the run said.
    let reduce = |s: &mut Session, p: &Value| -> (Vec<Vec<f32>>, Vec<String>) {
        let mark = s.notice_mark();
        let r = s.run("cube.reduce", p).unwrap_or_else(|e| panic!("{p}: {e}"));
        assert_eq!(r["layers"].as_array().unwrap().len(), 1, "{r}");
        let l = s.map().layer(r["layers"][0].as_u64().unwrap() as u32).unwrap();
        let raster = l.raster.as_ref().unwrap().data.clone().unwrap();
        let names: Vec<&str> = raster.bands.iter().map(|b| b.name.as_str()).collect();
        let assets: Vec<&str> = p["assets"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
        assert_eq!(names, assets);
        // Three bands are drawn as RGB, other counts not.
        assert_eq!(matches!(raster.color, rustgis_data::raster::ColorHint::Rgb { alpha: false }), assets.len() == 3, "{p}");
        let said = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        (raster.bands.iter().map(|b| b.data.clone()).collect(), said)
    };
    // Each cell's bands as `want` says, at (col, row).
    let check = |got: &[Vec<f32>], want: &dyn Fn(usize, usize) -> [f32; 3], what: &str| {
        for c in 0..64 {
            let (col, row) = (c % 8, c / 8);
            let w = want(col, row);
            for (j, band) in got.iter().enumerate() {
                assert!((band[c] - w[j]).abs() < 1e-3, "{what} ({col}, {row}) band {j}: {} vs {}", band[c], w[j]);
            }
        }
    };
    // The dates clear at a cell with a value in every band, in date order.
    let clear = |col: usize, row: usize| -> Vec<usize> {
        (0..3).filter(|k| !(*k == 0 && col == 0 || *k == 2 && col == 7 || *k == 1 && (col, row) == (3, 3))).collect()
    };
    // The median: the date whose bands' sum is the middle, the lower of two: one date's colour.
    let (median, said) = reduce(&mut s, &with(json!({"stat": "median"})));
    let middle = |col: usize, row: usize| {
        let mut dates = clear(col, row);
        dates.sort_by(|a, b| RGB[*a].iter().sum::<f32>().total_cmp(&RGB[*b].iter().sum()));
        RGB[dates[(dates.len() - 1) / 2]]
    };
    check(&median, &middle, "median");
    assert_eq!(median[0][9], 10.0, "three dates: date 0's, not the band's own middle (20)");
    assert!(said.iter().any(|t| t.contains("2 item(s) left out") && t.contains("B04, B03, B02")), "{said:?}");
    assert!(said.iter().any(|t| t.contains("1 pixel(s) had a value in some of the bands")), "{said:?}");
    // First, the clearest date first (the second, then the first and third, tied, by date).
    let (first, _) = reduce(&mut s, &with(json!({"stat": "first"})));
    check(&first, &|col, row| RGB[*[1, 0, 2].iter().find(|k| clear(col, row).contains(k)).unwrap()], "first, clearest");
    let (first, _) = reduce(&mut s, &with(json!({"stat": "first", "order": "date"})));
    check(&first, &|col, row| RGB[clear(col, row)[0]], "first, by date");
    // The mean of each band over the same dates.
    let (mean, _) = reduce(&mut s, &with(json!({"stat": "mean"})));
    let average = |col: usize, row: usize| {
        let d = clear(col, row);
        [0, 1, 2].map(|j| d.iter().map(|k| RGB[*k][j]).sum::<f32>() / d.len() as f32)
    };
    check(&mean, &average, "mean");
    // Two bands without the missing value: nothing left out, nothing said of it.
    let (_, said) = reduce(&mut s, &with(json!({"stat": "median", "assets": ["B04", "B03"]})));
    assert!(!said.iter().any(|t| t.contains("pixel(s) had a value")), "{said:?}");
    // What can't be asked.
    for (bad, why) in [
        (json!({"stat": "max"}), "assets takes mean, median or first"),
        (json!({"stat": "first", "asset": "B04"}), "not both"),
        (json!({"stat": "first", "assets": ["B04"]}), "2 to 16"),
        (json!({"stat": "first", "assets": ["B04", "b04"]}), "names b04 twice"),
        (json!({"stat": "first", "assets": ["B04", "B05"]}), "not on the pixels"),
        (json!({"stat": "first", "assets": ["B04", "B09"]}), "no item of s2 has all the assets B04, B09"),
    ] {
        let e = s.run("cube.reduce", &with(bad.clone())).unwrap_err();
        assert!(e.contains(why), "{bad}: {e}");
    }
    let e = s.run("cube.trend", &with(json!({}))).unwrap_err();
    assert!(e.contains("of one band"), "{e}");
    // A catalog entry's band gives way to the bands asked for; one asked for alongside it doesn't.
    let entry = json!({"cube": "bdc-s2-16d-ndvi", "assets": ["B04", "B03"]});
    let q = crate::cmd::cube_catalog::with_entry(&entry).unwrap();
    assert_eq!((q["collection"].as_str(), q.get("asset")), (Some("S2-16D-2"), None));
    let e =
        crate::cmd::cube_catalog::with_entry(&json!({"cube": "bdc-s2-16d-ndvi", "asset": "B04", "assets": ["B04", "B03"]})).unwrap_err();
    assert!(e.contains("leave out asset"), "{e}");
}

/// A stack of layers has no assets to read together: refused.
#[test]
fn assets_are_a_collections() {
    let mut s = Session::new(Project::default());
    let e = s.run("cube.reduce", &json!({"layers": ["a", "b"], "assets": ["B04", "B03"], "stat": "median"})).unwrap_err();
    assert!(e.contains("STAC collection's bands"), "{e}");
}

/// Averaged cells (`"resampling": "average"`) take each pixel with all its bands or not at all:
/// in the bottom half B04 has a value only in even columns and B03 only in odd ones, so no pixel
/// there has both and its cells are left out (said), not B04's even pixels' mean beside B03's
/// odd pixels' (#2712 review).
#[test]
fn an_averaged_cell_takes_its_bands_from_the_same_pixels() {
    let n = 1024u32;
    let band = |v: f32, even: bool| {
        cog((0..n * n).map(|c| if c / n < n / 2 || (c % n).is_multiple_of(2) == even { v } else { -1.0 }).collect(), n, Some(-1.0))
    };
    let (files, _) = serve_ranged_files(vec![("/B04.tif".into(), band(10.0, true)), ("/B03.tif".into(), band(20.0, false))]);
    let search = format!(
        r#"{{"type": "FeatureCollection", "features": [{{"type": "Feature", "id": "s0", "collection": "s2", "bbox": [10, 45, 11, 46],
           "geometry": null, "properties": {{"datetime": "2024-01-01T00:00:00Z"}},
           "assets": {{"B04": {{"href": "{files}/B04.tif"}}, "B03": {{"href": "{files}/B03.tif"}}}}}}]}}"#
    );
    let api = format!("{}/v1", serve_routes(vec![("/v1/search", search)], 64));
    let mut s = Session::new(Project::default());
    let mark = s.notice_mark();
    let p = json!({"url": api, "collection": "s2", "assets": ["B04", "B03"], "bbox": [10, 45, 11, 46], "start": "2024-01-01",
        "end": "2024-01-31", "stat": "first", "cellSize": 2.0 / f64::from(n), "resampling": "average"});
    let r = s.run("cube.reduce", &p).unwrap_or_else(|e| panic!("{e}"));
    let l = s.map().layer(r["layers"][0].as_u64().unwrap() as u32).unwrap();
    let raster = l.raster.as_ref().unwrap().data.clone().unwrap();
    let half = n as usize / 2;
    assert_eq!((raster.width, raster.height), (half as u32, half as u32));
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    for (j, want) in [10.0f32, 20.0].into_iter().enumerate() {
        let b = &raster.bands[j].data;
        assert!(b[..half * half / 2].iter().all(|v| (v - want).abs() < 1e-3), "band {j}, top half");
        assert!(b[half * half / 2..].iter().all(|v| v.is_nan()), "band {j}, bottom half: {:?} ({said:?})", &b[half * half / 2..][..4]);
    }
    assert!(said.iter().any(|t| t.contains("had a value in some of the bands")), "{said:?}");
}
