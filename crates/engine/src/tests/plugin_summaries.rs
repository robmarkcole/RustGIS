//! A plugin command's `cube.reduce` or `cube.trend` on a STAC collection, run by the app, reads its
//! slices off the frame, and the command runs again once they have landed (#3183).
use super::*;

/// A collection "s2" of `dates` dates of a 4 × 4 band over [10, 11] × [45, 46], date k's cells all
/// 10 (k + 1): its STAC API's address.
fn collection(dates: usize) -> String {
    collection_of(dates, 0.25)
}

/// A collection "s2" of `dates` dates of a 4 × 4 band of `pixel`° cells from (10, 46), date k's
/// cells all 10 (k + 1), its items over [10, 11] × [45, 46]: its STAC API's address. The band is
/// each item's B04, and its B08 and B11 too.
fn collection_of(dates: usize, pixel: f64) -> String {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let band = |k: usize| {
        let t = GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: pixel, pixel_height: -pixel };
        let r = Raster::new("r", 4, 4, vec![vec![10.0 * (k + 1) as f32; 16]], t, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        rustgis_data::raster::cogwrite::write(&r, 0).unwrap()
    };
    let (files, _) = serve_ranged_files((0..dates).map(|k| (format!("/b{k}.tif"), band(k))).collect());
    let features: Vec<String> = (0..dates)
        .map(|k| {
            let d = format!("2024-{:02}-01", k + 1);
            format!(
                r#"{{"type": "Feature", "id": "s{k}", "collection": "s2", "bbox": [10, 45, 11, 46], "geometry": null,
                   "properties": {{"datetime": "{d}T00:00:00Z", "bdc:tiles": ["t1"]}},
                   "assets": {{"B04": {{"href": "{files}/b{k}.tif"}}, "B08": {{"href": "{files}/b{k}.tif"}},
                              "B11": {{"href": "{files}/b{k}.tif"}}}}}}"#
            )
        })
        .collect();
    let search = format!(r#"{{"type": "FeatureCollection", "features": [{}]}}"#, features.join(","));
    format!("{}/v1", serve_routes(vec![("/v1/search", search)], 64))
}

/// `cube.reduce`'s parameters for the collection at `api`, with `stat`.
fn reduce(api: &str, stat: &str) -> Value {
    json!({"url": api, "collection": "s2", "asset": "B04", "bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-12-31", "stat": stat})
}

/// The plugin commands run again once their reads have landed, waited for (they read on threads).
fn ran_again(s: &mut Session) -> Vec<(String, crate::CmdResult)> {
    let started = web_time::Instant::now();
    loop {
        let ran = s.poll_plugin_retries();
        if !ran.is_empty() || started.elapsed() > web_time::Duration::from_secs(60) {
            return ran;
        }
        std::thread::sleep(std::time::Duration::from_millis(10)); // test-wait: the reads land on their threads, polled as the frame loop polls them
    }
}

/// The first cell of layer `id`'s first band.
fn first_cell(s: &Session, id: &Value) -> f32 {
    let l = s.map().layer(u32::try_from(id.as_u64().unwrap()).unwrap()).unwrap();
    l.raster.as_ref().unwrap().data.as_ref().unwrap().bands[0].data[0]
}

/// The call answers pending and adds nothing; the read runs off the frame, named for the plugin's
/// command, and the command runs again once it has landed, adding the layer as one undo step.
/// Clicked again meanwhile, it waits for the same read. Run another way (the command line, a
/// script), the same command waits for the read, and a summary of layers is read in the call.
#[test]
fn a_plugins_summary_of_a_collection_is_read_off_the_frame() {
    let mut s = world();
    plugin("eng-summary", &["edit", "files", "network"]);
    // Its collection is served from this computer.
    fetch_mode(true, true);
    let api = collection(3);
    let (undo, layers) = (s.undo.len(), s.map().layers.len());
    let p = json!({"op": "cube.reduce", "params": reduce(&api, "mean")});
    assert_eq!(s.run_leaving_downloads("plugin:eng-summary/f", &p).unwrap(), json!({"pending": true, "reading": ["cube.reduce"]}));
    assert_eq!((s.undo.len(), s.map().layers.len()), (undo, layers), "nothing added, and no undo step");
    assert!(s.take_url_requests().is_empty(), "nothing for the app to download");
    let under_way = s.plugin_summaries();
    assert_eq!(under_way.len(), 1);
    assert_eq!(under_way[0].command, "plugin:eng-summary/f");
    s.run_leaving_downloads("plugin:eng-summary/f", &p).unwrap();
    assert_eq!(s.plugin_summaries().len(), 1, "clicked again, the same read");
    let ran = ran_again(&mut s);
    assert_eq!(ran.len(), 1, "{ran:?}");
    assert_eq!(ran[0].0, "plugin:eng-summary/f");
    let answer = ran[0].1.as_ref().unwrap();
    assert_eq!(answer["dates"], 3, "{answer}");
    assert_eq!(first_cell(&s, &answer["layers"][0]), 20.0, "the mean of 10, 20 and 30");
    assert_eq!((s.undo.len(), s.map().layers.len()), (undo + 1, layers + 1), "one layer, one undo step");
    assert!(s.plugin_summaries().is_empty() && !s.plugin_retries_waiting());
    assert!(s.poll_plugin_retries().is_empty(), "run once");
    // Run another way, it waits for the read.
    let max = s.run("plugin:eng-summary/f", &json!({"op": "cube.reduce", "params": reduce(&api, "max")})).unwrap();
    assert_eq!(first_cell(&s, &max["layers"][0]), 30.0);
    assert!(s.plugin_summaries().is_empty() && !s.plugin_retries_waiting());
    // A summary of layers is read in the call, as before.
    let names: Vec<String> = [&answer["layers"][0], &max["layers"][0]]
        .iter()
        .map(|id| s.map().layer(id.as_u64().unwrap() as u32).unwrap().name.clone())
        .collect();
    let q = json!({"op": "cube.reduce", "params": {"layers": names, "dates": ["2024-01-01", "2024-02-01"], "stat": "min"}});
    let min = s.run_leaving_downloads("plugin:eng-summary/f", &q).unwrap();
    assert_eq!(first_cell(&s, &min["layers"][0]), 20.0, "{min}");
    assert!(s.plugin_summaries().is_empty() && !s.plugin_retries_waiting());
    rustgis_plugins::unload("eng-summary");
}

/// Two summaries in one call are both left, read one after the other, and the command runs again
/// once, when both have landed: both layers in one undo step.
#[test]
fn two_summaries_in_one_call_run_the_command_again_once() {
    let mut s = world();
    fetch_mode(true, true);
    let api = collection(3);
    let id = "eng-summary-two";
    let calls = [json!({"op": "cube.reduce", "params": reduce(&api, "min")}), json!({"op": "cube.reduce", "params": reduce(&api, "max")})];
    let wasm = rustgis_plugins::testing::wasm_with_host_calls(&calls);
    assert!(!wasm.is_empty());
    let manifest = rustgis_plugins::testing::manifest(id, &["edit", "files", "network"]);
    let package = rustgis_plugins::testing::zip_of(&[("plugin.json", manifest.as_bytes()), ("plugin.wasm", &wasm)]);
    rustgis_plugins::load(rustgis_plugins::Package::from_zip(&package).unwrap(), "test").unwrap();
    let (undo, layers) = (s.undo.len(), s.map().layers.len());
    let answer = s.run_leaving_downloads(&format!("plugin:{id}/f"), &json!({})).unwrap();
    assert_eq!(answer, json!({"pending": true, "reading": ["cube.reduce", "cube.reduce"]}));
    assert_eq!(s.plugin_summaries().len(), 2);
    let ran = ran_again(&mut s);
    assert_eq!(ran.len(), 1, "{ran:?}");
    // The module answers its last call's reply: the max.
    let max = ran[0].1.as_ref().unwrap();
    assert_eq!(first_cell(&s, &max["layers"][0]), 30.0, "{max}");
    assert_eq!((s.undo.len(), s.map().layers.len()), (undo + 1, layers + 2));
    let min = s.map().layers.iter().find(|l| l.name.contains(" min ")).map(|l| json!(l.id)).unwrap();
    assert_eq!(first_cell(&s, &min), 10.0);
    assert!(s.plugin_summaries().is_empty() && !s.plugin_retries_waiting());
    rustgis_plugins::unload(id);
}

/// Cancelled, the read answers the command's call run again with that error, and nothing is added.
#[test]
fn a_cancelled_summary_is_the_calls_error() {
    let mut s = world();
    plugin("eng-summary-cancel", &["edit", "files", "network"]);
    fetch_mode(true, true);
    let api = collection(3);
    let (undo, layers) = (s.undo.len(), s.map().layers.len());
    let p = json!({"op": "cube.reduce", "params": reduce(&api, "mean")});
    // Held before it starts, so the Cancel comes first.
    let turn = crate::cmd::plugins::summary::hold_turn();
    s.run_leaving_downloads("plugin:eng-summary-cancel/f", &p).unwrap();
    let under_way = s.plugin_summaries();
    s.cancel_plugin_summary(under_way[0].id);
    assert!(s.plugin_summaries()[0].cancelled);
    drop(turn);
    let ran = ran_again(&mut s);
    assert_eq!(ran.len(), 1, "{ran:?}");
    assert!(ran[0].1.as_ref().is_err_and(|e| e.contains("cube.reduce was cancelled")), "{ran:?}");
    assert_eq!((s.undo.len(), s.map().layers.len()), (undo, layers), "nothing added");
    assert!(s.plugin_summaries().is_empty() && !s.plugin_retries_waiting());
    rustgis_plugins::unload("eng-summary-cancel");
}

/// A trend over a collection is read off the frame the same way; its parameters are checked in the
/// call, so one that can't be worked out is said at once.
#[test]
fn a_plugins_trend_of_a_collection_is_read_off_the_frame() {
    let mut s = world();
    plugin("eng-summary-trend", &["edit", "files", "network"]);
    fetch_mode(true, true);
    let api = collection(6);
    let mut p = reduce(&api, "mean");
    if let Some(o) = p.as_object_mut() {
        o.remove("stat");
    }
    // Two dates: too few for a trend, said in the call, and nothing left to read.
    let mut short = p.clone();
    short["end"] = json!("2024-02-28");
    let e = s.run_leaving_downloads("plugin:eng-summary-trend/f", &json!({"op": "cube.trend", "params": short})).unwrap_err();
    assert!(e.contains("a trend needs"), "{e}");
    assert!(s.plugin_summaries().is_empty() && !s.plugin_retries_waiting());
    let layers = s.map().layers.len();
    let answer = s.run_leaving_downloads("plugin:eng-summary-trend/f", &json!({"op": "cube.trend", "params": p})).unwrap();
    assert_eq!(answer, json!({"pending": true, "reading": ["cube.trend"]}));
    let ran = ran_again(&mut s);
    let answer = ran[0].1.as_ref().unwrap();
    assert_eq!(answer["layers"].as_array().map(Vec::len), Some(4), "{answer}");
    assert_eq!(s.map().layers.len(), layers + 4);
    rustgis_plugins::unload("eng-summary-trend");
}

/// Loads plugin `id`, whose commands make `commands`' calls (each its list, by its one-letter id).
fn calls_plugin(id: &str, commands: &[(char, &[Value])]) {
    let wasm = rustgis_plugins::testing::wasm_with_host_calls_by_command(commands);
    assert!(!wasm.is_empty());
    let manifest = rustgis_plugins::testing::manifest(id, &["edit", "files", "network"]);
    let package = rustgis_plugins::testing::zip_of(&[("plugin.json", manifest.as_bytes()), ("plugin.wasm", &wasm)]);
    rustgis_plugins::load(rustgis_plugins::Package::from_zip(&package).unwrap(), "test").unwrap();
}

/// `cube.reduce` of the collection at `api` with `stat`, as a plugin's call.
fn reduce_call(api: &str, stat: &str) -> Value {
    json!({"op": "cube.reduce", "params": reduce(api, stat)})
}

/// Two commands waiting for one read add what it read without copying it: their layers hold the
/// same pixels and GeoTIFF, made on the read's thread.
#[test]
fn commands_sharing_a_summary_share_its_pixels() {
    let mut s = world();
    fetch_mode(true, true);
    let api = collection(3);
    let id = "eng-summary-shared";
    let mean = [reduce_call(&api, "mean")];
    calls_plugin(id, &[('h', &mean), ('f', &mean)]);
    // Held back, so both commands ask for it before it lands.
    let turn = crate::cmd::plugins::summary::hold_turn();
    for command in ["h", "f"] {
        let answer = s.run_leaving_downloads(&format!("plugin:{id}/{command}"), &json!({})).unwrap();
        assert_eq!(answer, json!({"pending": true, "reading": ["cube.reduce"]}), "{command}");
    }
    assert_eq!(s.plugin_summaries().len(), 1, "one read for both");
    drop(turn);
    let ran = ran_again(&mut s);
    assert_eq!(ran.len(), 2, "{ran:?}");
    let raster = |answer: &crate::CmdResult| {
        let id = u32::try_from(answer.as_ref().unwrap()["layers"][0].as_u64().unwrap()).unwrap();
        s.map().layer(id).unwrap().raster.clone().unwrap()
    };
    let (a, b) = (raster(&ran[0].1), raster(&ran[1].1));
    assert!(std::sync::Arc::ptr_eq(a.data.as_ref().unwrap(), b.data.as_ref().unwrap()), "the same pixels");
    assert!(std::sync::Arc::ptr_eq(a.embedded.as_ref().unwrap(), b.embedded.as_ref().unwrap()), "the same GeoTIFF");
    rustgis_plugins::unload(id);
}

/// A summary that has landed, its command still waiting for another of its requests, counts among
/// those held: no more than [`MAX_SUMMARIES`] are held at once.
///
/// [`MAX_SUMMARIES`]: crate::cmd::plugins::summary::MAX_SUMMARIES
#[test]
fn a_summary_landed_and_held_counts_among_those_read() {
    let mut s = world();
    fetch_mode(true, true);
    let api = collection(3);
    let id = "eng-summary-held";
    let first = [reduce_call(&api, "mean"), json!({"op": "layer.addUrl", "params": {"url": "https://x.org/held.csv"}})];
    let more: Vec<Value> = ["min", "max", "sum", "std"].iter().map(|stat| reduce_call(&api, stat)).collect();
    calls_plugin(id, &[('f', &first), ('h', &more)]);
    let answer = s.run_leaving_downloads(&format!("plugin:{id}/f"), &json!({})).unwrap();
    assert_eq!(answer["reading"], json!(["cube.reduce"]), "{answer}");
    // It lands; the command still waits for its download.
    let started = web_time::Instant::now();
    while !s.plugin_summaries().is_empty() && started.elapsed() < web_time::Duration::from_secs(60) {
        assert!(s.poll_plugin_retries().is_empty());
        std::thread::sleep(std::time::Duration::from_millis(10)); // test-wait: the read lands on its thread, polled as the frame loop polls it
    }
    assert!(s.plugin_summaries().is_empty() && s.plugin_retries_waiting());
    // Four more would hold five.
    let e = s.run_leaving_downloads(&format!("plugin:{id}/h"), &json!({})).unwrap_err();
    assert!(e.contains("already being read"), "{e}");
    assert!(s.plugin_summaries().is_empty(), "none of them started");
    rustgis_plugins::unload(id);
}

/// A read asked for again by the next call, beside new ones, is counted once: with two under way,
/// a call asking for one of them and two more holds four, and is let through.
#[test]
fn a_summary_asked_again_is_counted_once() {
    let mut s = world();
    fetch_mode(true, true);
    let api = collection(3);
    let id = "eng-summary-again";
    let (a, b, c, d) = (reduce_call(&api, "mean"), reduce_call(&api, "min"), reduce_call(&api, "max"), reduce_call(&api, "sum"));
    calls_plugin(id, &[('h', &[a.clone(), b]), ('f', &[a, c, d])]);
    let turn = crate::cmd::plugins::summary::hold_turn();
    s.run_leaving_downloads(&format!("plugin:{id}/h"), &json!({})).unwrap();
    assert_eq!(s.plugin_summaries().len(), 2);
    let answer = s.run_leaving_downloads(&format!("plugin:{id}/f"), &json!({})).unwrap();
    assert_eq!(answer["reading"].as_array().map(Vec::len), Some(3), "{answer}");
    assert_eq!(s.plugin_summaries().len(), 4);
    drop(turn);
    let ran = ran_again(&mut s);
    assert!(!ran.is_empty(), "they land");
    rustgis_plugins::unload(id);
}

/// A project replacing this one stops its summaries: a summary of the new project isn't held
/// behind them.
#[test]
fn a_new_project_stops_the_summaries_of_the_old() {
    let mut s = world();
    plugin("eng-summary-new-project", &["edit", "files", "network"]);
    fetch_mode(true, true);
    let api = collection(3);
    // A read of the old project that lasts until it is stopped.
    s.start_test_summary("plugin:eng-summary-new-project/f");
    s.run("project.sample", &Value::Null).unwrap();
    assert!(s.plugin_summaries().is_empty(), "not the new project's");
    s.run_leaving_downloads("plugin:eng-summary-new-project/f", &reduce_call(&api, "mean")).unwrap();
    let ran = ran_again(&mut s);
    assert_eq!(ran.len(), 1, "the new project's read lands: {ran:?}");
    assert!(ran[0].1.is_ok(), "{ran:?}");
    rustgis_plugins::unload("eng-summary-new-project");
}

/// A command that replaced the project and then failed gives the old one back: the summaries the
/// replacement started stop with it, so the old project's next one isn't held behind them.
#[test]
fn a_project_given_back_stops_the_summaries_of_its_replacement() {
    let mut s = world();
    plugin("eng-summary-given-back", &["edit", "files", "network"]);
    fetch_mode(true, true);
    let api = collection(3);
    let e = s.guarded("test", true, |s| {
        s.run("project.sample", &Value::Null)?;
        // A read of the replacement that lasts until it is stopped.
        s.start_test_summary("plugin:eng-summary-given-back/f");
        Err("failed once the project was replaced".to_string())
    });
    assert!(e.is_err());
    assert!(s.plugin_summaries().is_empty(), "not the replacement's");
    s.run_leaving_downloads("plugin:eng-summary-given-back/f", &reduce_call(&api, "mean")).unwrap();
    let ran = ran_again(&mut s);
    assert_eq!(ran.len(), 1, "the old project's read lands: {ran:?}");
    assert!(ran[0].1.is_ok(), "{ran:?}");
    rustgis_plugins::unload("eng-summary-given-back");
}

/// A trend over a large grid plans all four of its outputs, with their GeoTIFFs: its run doesn't
/// cap them, only its area (`MAX_TREND_CELLS`).
#[test]
fn a_large_trend_plans_its_whole_output() {
    let mut s = world();
    fetch_mode(true, true);
    // 1° in cells of 0.000201°: about 4,976 × 4,976 cells, within a trend's area.
    let api = collection_of(4, 0.000201);
    let mut p = reduce(&api, "mean");
    p.as_object_mut().unwrap().remove("stat");
    let run = crate::cmd::cube::trend_start(&mut s, &p).unwrap();
    let whole = 4_975u64 * 4_975 * 4 * 8;
    assert!(run.output_bytes() >= whole, "{} bytes planned, not the {whole} its outputs hold", run.output_bytes());
}

/// A command that failed leaves the summaries as they were before it: one it started stops, so the
/// next isn't held behind it.
#[test]
fn a_failed_command_stops_the_summaries_it_started() {
    let mut s = world();
    plugin("eng-summary-failed", &["edit", "files", "network"]);
    fetch_mode(true, true);
    let api = collection(3);
    let e = s.guarded("test", true, |s| {
        // A read that lasts until it is stopped.
        s.start_test_summary("plugin:eng-summary-failed/f");
        Err("failed once the read had started".to_string())
    });
    assert!(e.is_err());
    assert!(s.plugin_summaries().is_empty(), "not the failed command's");
    s.run_leaving_downloads("plugin:eng-summary-failed/f", &reduce_call(&api, "mean")).unwrap();
    let ran = ran_again(&mut s);
    assert_eq!(ran.len(), 1, "the next read lands: {ran:?}");
    assert!(ran[0].1.is_ok(), "{ran:?}");
    rustgis_plugins::unload("eng-summary-failed");
}

/// A summary of several bands plans an output band for each, but firstDate one for all of them:
/// one date a cell, the same for its bands.
#[test]
fn a_first_date_of_several_bands_plans_one_output() {
    let mut s = world();
    fetch_mode(true, true);
    let api = collection(3);
    let planned = |s: &mut Session, stat: &str| {
        let mut p = reduce(&api, stat);
        let o = p.as_object_mut().unwrap();
        o.remove("asset");
        o.insert("assets".into(), json!(["B04", "B08", "B11"]));
        crate::cmd::cube::reduce_start(s, &p).unwrap().output_bytes()
    };
    // 4 × 4 cells, 8 bytes each for the raster and its GeoTIFF.
    assert_eq!(planned(&mut s, "mean"), 16 * 3 * 8, "a band each");
    assert_eq!(planned(&mut s, "firstDate"), 16 * 8, "one for all three");
}
