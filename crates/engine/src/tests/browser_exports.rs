//! Exports by command where files are downloaded: the browser has no filesystem to write a path
//! in, so a script's, the Assistant's or a plugin's export is offered as a download instead.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustgis_data::source::{Batch, FeatureSource, Request, SourceInfo};
use rustgis_data::{Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value as Cell};
use serde_json::{Value, json};

use super::write_notices::Files;
use crate::{CmdResult, Project, Session};

/// A point layer, a standalone table and a raster, a layout and a report. A field name longer
/// than a Shapefile holds and a NaN, which GeoJSON can't, give exports something to say.
/// `browser` is whether files are downloaded.
fn session(files: &Files, browser: bool) -> Session {
    let mut s = Session::new(Project::default());
    let fields = vec![Field::new("long_attribute_name", FieldType::Text), Field::new("score", FieldType::Double)];
    let mut ds = Dataset::new("Places", GeometryType::Point, fields.clone());
    ds.crs = rustgis_data::raster::Crs::from_epsg(4326);
    ds.push(Some(Geometry::Point(rustgis_data::Coord::new(1.0, 2.0))), vec![Cell::Text("kept".into()), Cell::Float(f64::NAN)]);
    s.map_mut().add_dataset(ds);
    let mut totals = Dataset::new("Totals", GeometryType::None, fields);
    totals.push(None, vec![Cell::Text("a".into()), Cell::Float(1.5)]);
    s.project.add_table(totals);
    let grid = files.path().join("dem.asc");
    std::fs::write(&grid, "ncols 2\nnrows 2\nxllcorner 0\nyllcorner 0\ncellsize 1\n1 2\n3 4\n").unwrap();
    s.run("layer.addData", &json!({"path": grid, "name": "Dem"})).unwrap();
    s.run("layout.new", &json!({"name": "Page", "template": "blank"})).unwrap();
    s.run("report.new", &json!({"layer": "Places", "name": "List"})).unwrap();
    s.queue_gp_files = browser;
    s
}

/// `params` writing to `path`.
fn to(params: &Value, path: &str) -> Value {
    let mut p = params.clone();
    p["path"] = json!(path);
    p
}

/// The notices said since `mark`.
fn said(s: &Session, mark: u64) -> Vec<String> {
    s.notices_since(mark).iter().map(|n| n.text.clone()).collect()
}

/// Takes the one queued download and acknowledges it as the app does: its name, its bytes and
/// the acknowledgment's answer.
fn deliver(s: &mut Session, pending: &Value, error: Option<&str>) -> (String, Vec<u8>, CmdResult) {
    assert_eq!(pending["pending"], true, "{pending}");
    let mut files = s.take_gp_files();
    assert_eq!(files.len(), 1, "{pending}");
    let (id, name, bytes) = files.remove(0);
    assert_eq!(pending["download"].as_u64(), Some(id));
    let answer = s.acknowledge_gp_file(id, error);
    (name, bytes.to_vec(), answer)
}

/// Each export command the browser can run downloads the file the desktop writes, under the
/// path's name, and says of it what the desktop says, once the download is taken.
#[test]
fn a_browser_export_by_path_downloads_the_desktops_file_and_says_the_same() {
    // Command, other parameters, file name, and whether two writes of it give the same bytes.
    let cases = [
        ("share.exportTable", json!({"table": "Totals"}), "totals.csv", true),
        ("share.exportTable", json!({"table": "Totals", "limit": 0}), "totals.geojson", true),
        ("share.exportLayer", json!({"layer": "Places"}), "z.geojson", true),
        ("share.exportLayer", json!({"layer": "Places"}), "z.csv", true),
        ("share.exportLayer", json!({"layer": "Places"}), "z.zip", true),
        ("share.exportLayer", json!({"layer": "Places", "crs": "EPSG:3857"}), "z.parquet", true),
        ("share.exportLayer", json!({"layer": "Places"}), "z.kml", true),
        ("share.exportLayer", json!({"layer": "Places"}), "z.kmz", true),
        ("share.exportLayer", json!({"layer": "Places"}), "z.gpx", true),
        ("data.convert", json!({"layer": "Places", "format": "wkt"}), "z.txt", true),
        ("data.convert", json!({"layer": "Places", "format": "csv"}), "z.csv", true),
        ("share.exportRaster", json!({"layer": "Dem"}), "dem.tif", true),
        ("share.exportRaster", json!({"layer": "Dem", "cog": true}), "cog.tif", true),
        ("share.exportMap", json!({"width": 64, "height": 64}), "map.png", true),
        ("share.exportMap", json!({"width": 64, "height": 64}), "map.tif", true),
        ("share.exportMap", json!({"width": 64, "height": 64, "cog": true}), "mapcog.tif", true),
        ("layout.export", json!({"layout": "Page", "dpi": 72}), "page.pdf", false),
        ("report.export", json!({"report": "List", "dpi": 72}), "list.pdf", false),
    ];
    // A new export is listed here, or among those refused.
    for (command, ..) in crate::cmd::exports::IN_BACKGROUND {
        assert!(cases.iter().any(|c| c.0 == *command) || REFUSED.iter().any(|r| r.0 == *command), "{command}");
    }
    for (command, params, file, same_bytes) in cases {
        let files = Files::new();
        let mut desktop = session(&files, false);
        let path = files.path().join(file);
        let path = path.to_str().unwrap();
        let mark = desktop.notice_mark();
        let wrote = desktop.run(command, &to(&params, path)).unwrap_or_else(|e| panic!("{command} {file}: {e}"));
        let written = std::fs::read(path).unwrap();
        let folder = format!("{}{}", files.path().display(), std::path::MAIN_SEPARATOR);
        let desktop_said: Vec<String> = said(&desktop, mark).into_iter().map(|n| n.replace(&folder, "")).collect();
        // A layer's export says it was exported, and what its format couldn't hold.
        if command == "share.exportLayer" {
            assert!(desktop_said.contains(&format!("Exported {file}")), "{file}: {desktop_said:?}");
        }
        if file == "z.geojson" {
            assert!(desktop_said.iter().any(|n| n.contains("written as null")), "{desktop_said:?}");
        }
        if file == "z.zip" {
            assert!(desktop_said.iter().any(|n| n.contains("long_attribute_name")), "{desktop_said:?}");
        }

        let mut browser = session(&files, true);
        std::fs::remove_file(path).unwrap();
        let (mark, steps) = (browser.notice_mark(), browser.undo_depth());
        let pending = browser.run(command, &to(&params, file)).unwrap_or_else(|e| panic!("{command} {file}: {e}"));
        assert_eq!(said(&browser, mark), Vec::<String>::new(), "{command} {file}: nothing is said before the download");
        let (name, bytes, answer) = deliver(&mut browser, &pending, None);
        assert_eq!(name, file);
        if same_bytes {
            assert!(bytes == written, "{command} {file}: {} bytes downloaded, {} written", bytes.len(), written.len());
        } else {
            assert_eq!(bytes.get(..4), written.get(..4), "{command} {file}");
        }
        assert_eq!(said(&browser, mark), desktop_said, "{command} {file}");
        let answer = answer.unwrap();
        assert_eq!((&answer["delivered"], &answer["file"]), (&json!(true), &json!(file)), "{answer}");
        for (key, value) in wrote.as_object().unwrap() {
            if key != "path" && (same_bytes || key != "bytes") {
                assert_eq!(&answer[key], value, "{command} {file}: {key}");
            }
        }
        assert_eq!(answer["path"], file);
        assert!(!std::path::Path::new(file).exists(), "{command} {file}: no file is written");
        assert!(browser.project.gp_history.is_empty(), "an export isn't a tool run");
        assert_eq!(browser.undo_depth(), steps, "{command} {file}");
    }
}

/// The reported script: each line's export is downloaded, its losses told to a caller that reads
/// only the answer.
#[test]
fn a_scripts_browser_exports_are_pending_downloads() {
    let files = Files::new();
    let mut s = session(&files, true);
    let script = "share.exportTable {\"table\": \"Totals\", \"path\": \"totals.csv\"}\n\
                  share.exportLayer {\"layer\": \"Places\", \"path\": \"z.geojson\"}";
    let mark = s.notice_mark();
    let ran = s.run("script.run", &json!({"script": script})).unwrap();
    let results: Vec<&Value> = ran["ran"].as_array().unwrap().iter().map(|r| &r["result"]).collect();
    assert_eq!(results.iter().map(|r| &r["file"]).collect::<Vec<_>>(), ["totals.csv", "z.geojson"]);
    let messages = results[1]["messages"].to_string();
    assert!(messages.contains("written as null") && !messages.contains("Exported"), "{messages}");
    assert_eq!(s.take_gp_files().len(), 2);
    assert_eq!(said(&s, mark), Vec::<String>::new());
}

/// What the download's name can't keep of the path is said, once the download is taken; a plain
/// file name says neither.
#[test]
fn a_browser_export_says_when_its_download_is_named_otherwise() {
    let files = Files::new();
    let mut s = session(&files, true);
    for (command, path, file, zipped, folder) in [
        ("share.exportLayer", "z.geojson", "z.geojson", false, false),
        ("share.exportLayer", "z.zip", "z.zip", false, false),
        ("share.exportLayer", "Roads.SHP", "Roads.zip", true, false),
        ("share.exportLayer", "/home/me/out/rows.shp", "rows.zip", true, true),
        ("share.exportTable", "out\\rows.csv", "rows.csv", false, true),
    ] {
        let params = json!({"layer": "Places", "table": "Totals", "path": path});
        let mark = s.notice_mark();
        let pending = s.run(command, &params).unwrap();
        let (name, bytes, _) = deliver(&mut s, &pending, None);
        assert_eq!(name, file, "{path}");
        assert_eq!(bytes.starts_with(b"PK"), file.ends_with(".zip"), "{path}");
        let notices = said(&s, mark);
        let zip_note = format!("{file}: a Shapefile is several files, so the browser downloads them in one .zip");
        let folder_note = format!("{file}: the browser chooses where downloads go, so the folder in '{path}' wasn't used");
        assert_eq!(notices.contains(&zip_note), zipped, "{path}: {notices:?}");
        assert_eq!(notices.contains(&folder_note), folder, "{path}: {notices:?}");
    }
}

/// What the browser can't write: the command, its path, and what its refusal says to do.
const REFUSED: &[(&str, &str, &str)] = &[
    ("share.exportLayer", "z.gpkg", "can't write GeoPackage or FlatGeobuf files; export to .geojson"),
    ("share.exportLayer", "z.FGB", "can't write GeoPackage or FlatGeobuf files; export to .geojson"),
    ("share.exportTable", "rows.gpkg", "can't write GeoPackage or FlatGeobuf files; export to .geojson"),
    ("share.exportLayer", "out/", "this one has none"),
    ("share.exportMap", "..", "this one has none"),
    ("offline.package", "site.rgisfield", "use Package for the Field on the ribbon"),
    ("map.exportOrbit", "spin.gif", "the web app can't export an orbit; open the project in the desktop app"),
    ("map.exportBlock", "block.glb", "the web app can't export a block; open the project in the desktop app"),
    ("source.publish", "unused", "publish from the desktop app"),
];

/// What the browser can't write is refused before anything is made, saying how to export instead.
#[test]
fn what_the_browser_cant_write_is_refused_up_front() {
    let files = Files::new();
    let mut s = session(&files, true);
    // A layer read from a file, which Publish to Source writes back to.
    let filed = files.path().join("filed.geojson");
    std::fs::write(&filed, r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"a":1},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#).unwrap();
    s.run("layer.addData", &json!({"path": filed, "name": "Filed"})).unwrap();
    for &(command, path, why) in REFUSED {
        let areas = s.project.offline_areas.len();
        let layer = if command == "source.publish" { "Filed" } else { "Places" };
        let params = json!({"layer": layer, "table": "Totals", "name": "Site", "path": path, "frames": 1, "width": 64, "height": 64});
        let params = if command == "source.publish" { json!({"layer": layer}) } else { params };
        let e = s.run(command, &params).unwrap_err();
        assert!(e.contains(why), "{command} {path}: {e}");
        assert_eq!(s.project.offline_areas.len(), areas);
        assert!(s.take_gp_files().is_empty());
        assert!(!std::path::Path::new(path).is_file(), "{command}: no file is written");
    }
}

/// A raster layer `name` of `bands` bands, and the GeoTIFF of its first one.
fn raster(name: &str, bands: usize) -> (rustgis_data::raster::Raster, Vec<u8>) {
    let transform = rustgis_data::raster::GeoTransform { origin_x: 0.0, origin_y: 2.0, pixel_width: 1.0, pixel_height: -1.0 };
    let r = rustgis_data::raster::Raster::new(
        name,
        2,
        2,
        vec![vec![1.0, 2.0, 3.0, 4.0]; bands],
        transform,
        rustgis_data::raster::Crs::LonLat,
        None,
        rustgis_data::raster::ColorHint::Gray,
        "Float32",
    )
    .unwrap();
    let tif = rustgis_data::raster::geotiff::write(&r, 0).unwrap();
    (r, tif)
}

/// A pending export doesn't claim its file is exported, whatever follows that claim in what the
/// file says; what it leaves out is still in the answer, and the claim is made on delivery.
#[test]
fn a_pending_raster_export_says_what_it_leaves_out_but_not_that_it_is_exported() {
    let files = Files::new();
    let mut s = session(&files, true);
    let (r, tif) = raster("Bands", 2);
    s.map_mut().add_raster("Bands", rustgis_map::RasterLayer::from_bytes(r, "bands.tif", tif));
    for (layer, file, lossy) in [("Bands", "bands.tif", true), ("Dem", "dem.tif", false)] {
        let mark = s.notice_mark();
        let pending = s.run("share.exportRaster", &json!({"layer": layer, "path": file})).unwrap();
        let messages: Vec<&str> = pending["messages"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(!messages.iter().any(|m| m.contains("Exported")), "{messages:?}");
        if lossy {
            assert_eq!(messages.len(), 1, "{messages:?}");
            assert!(messages[0].starts_with("bands.tif; 1 of its 2 bands written"), "{messages:?}");
        } else {
            assert_eq!(messages, Vec::<&str>::new());
        }
        deliver(&mut s, &pending, None).2.unwrap();
        let notices = said(&s, mark);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].starts_with(&format!("Exported {file}")), "{notices:?}");
        assert_eq!(notices[0].contains("1 of its 2 bands written"), lossy, "{notices:?}");
    }
}

/// A raster a command made is downloaded from the file its layer holds, with no second copy.
#[test]
fn a_raster_download_shares_the_layers_file() {
    let files = Files::new();
    let mut s = session(&files, true);
    let (r, tif) = raster("Made", 1);
    let id = s.map_mut().add_raster("Made", rustgis_map::RasterLayer::from_written(r, "made.tif", tif));
    let held = s.map().layer(id).unwrap().raster.as_ref().unwrap().written_file().unwrap().clone();
    s.run("share.exportRaster", &json!({"layer": "Made", "path": "made.tif"})).unwrap();
    let downloads = s.take_gp_files();
    assert_eq!(downloads.len(), 1);
    assert_eq!(downloads[0].2.as_ptr(), held.as_ptr(), "the layer's own bytes are handed over");
}

/// A source read in place which says it holds `count` features, or gives a row of `text` bytes,
/// as a large layer does, and notes whether it was read.
struct Large {
    info: SourceInfo,
    inner: Box<dyn FeatureSource>,
    text: usize,
    chunks: bool,
    read: Arc<AtomicBool>,
}

impl Large {
    /// The row of `text` bytes: its room is reserved, not filled.
    fn row(&self) -> Feature {
        let mut text = String::with_capacity(self.text);
        text.push('x');
        Feature { id: 1, geometry: Some(Geometry::Point(rustgis_data::Coord::new(1.0, 2.0))), attrs: vec![Cell::Text(text)] }
    }
}

impl FeatureSource for Large {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    /// The whole source in one read, as one that can't be read a chunk at a time answers.
    fn read(&self, req: &Request) -> rustgis_data::Result<Batch> {
        self.read.store(true, Ordering::Relaxed);
        if self.text == 0 {
            return self.inner.read(req);
        }
        Ok(Batch { features: vec![self.row()], matched: 1, complete: true, ..Batch::default() })
    }

    fn visits_chunks(&self) -> bool {
        self.chunks
    }

    /// The large row as one chunk, else the small file's rows.
    fn visit_chunks(
        &self,
        extent: Option<rustgis_geom::Extent>,
        bytes: usize,
        visit: &mut dyn FnMut(Vec<Feature>) -> rustgis_data::Result<()>,
    ) -> rustgis_data::Result<Batch> {
        self.read.store(true, Ordering::Relaxed);
        if self.text == 0 {
            return self.inner.visit_chunks(extent, bytes, visit);
        }
        visit(vec![self.row()])?;
        Ok(Batch { matched: 1, complete: true, ..Batch::default() })
    }
}

/// Adds layer `name`, read in place from a [`Large`] source; gives whether it has been read.
fn streamed(s: &mut Session, name: &str, count: Option<u64>, text: usize, chunks: bool) -> Arc<AtomicBool> {
    let mut ds = Dataset::new(name, GeometryType::Point, vec![Field::new("a", FieldType::Text)]);
    ds.push(Some(Geometry::Point(rustgis_data::Coord::new(1.0, 2.0))), vec![Cell::Text("kept".into())]);
    let bytes = Arc::new(rustgis_data::source::fgb::write(&ds).unwrap());
    let inner = rustgis_data::source::open_shared("large.fgb", bytes.clone(), None).unwrap();
    let mut info = inner.info().clone();
    info.count = count;
    let read = Arc::new(AtomicBool::new(false));
    let source = Large { info, inner, text, chunks, read: read.clone() };
    let source = rustgis_map::source::SourceLayer::from_bytes("large.fgb", bytes, None, Box::new(source));
    let mut layer = rustgis_map::Layer::new_streamed(s.map().layers.len() as u32 + 10, source);
    layer.name = name.into();
    s.map_mut().layers.push(layer);
    read
}

/// A layer read in place too large to hold while its download is made is refused: before any of
/// it is read when its source says how many features it holds, else as soon as what is read
/// passes the limit. A small one downloads.
#[test]
fn a_browser_export_of_a_large_streamed_layer_is_refused() {
    let files = Files::new();
    let mut s = session(&files, true);
    // Name, count the source says, bytes of its row, read a chunk at a time, read before refusal.
    let cases = [
        ("Many", Some(10_000_000), 0, true, false),
        ("ManyAtOnce", Some(10_000_000), 0, false, false),
        ("Heavy", None, 300 << 20, true, true),
        ("HeavyAtOnce", Some(1), 300 << 20, false, true),
    ];
    for (layer, count, text, chunks, reads) in cases {
        for (command, params, file) in [
            ("share.exportLayer", json!({"layer": layer}), "z.geojson"),
            ("data.convert", json!({"layer": layer, "format": "csv"}), "z.csv"),
        ] {
            let read = streamed(&mut s, layer, count, text, chunks);
            let e = s.run(command, &to(&params, file)).unwrap_err();
            assert!(e.contains(layer) && e.contains("more than the 256 MiB"), "{command} {layer}: {e}");
            assert!(e.contains("Make Layer From Selected Features") && e.contains("desktop app"), "{e}");
            assert_eq!(read.load(Ordering::Relaxed), reads, "{command} {layer}");
            assert!(s.take_gp_files().is_empty());
            s.map_mut().layers.retain(|l| l.name != layer);
        }
    }
    for chunks in [true, false] {
        let layer = format!("Small{chunks}");
        streamed(&mut s, &layer, Some(1), 0, chunks);
        let mark = s.notice_mark();
        let pending = s.run("share.exportLayer", &json!({"layer": layer, "path": "small.geojson"})).unwrap();
        let (_, bytes, _) = deliver(&mut s, &pending, None);
        assert!(String::from_utf8_lossy(&bytes).contains("kept"));
        assert_eq!(said(&s, mark), ["Exported small.geojson"]);
    }
}

/// Every export command that downloads, over the limit (a stand-in of ten bytes): its
/// writer stops once the file passes it, nothing waits to be downloaded, and the refusal says
/// how to export. A picture's size is bounded by its pixels, so a PNG, an SVG, and the file a
/// raster layer already holds are refused once made. Under the limit, each downloads.
#[test]
fn a_browser_export_over_the_limit_is_stopped_as_it_is_made() {
    const STOPPED: &str = "stopped once the file passed";
    const MADE: &str = "more than the 1 MiB a command can download";
    let cases = [
        ("share.exportTable", json!({"table": "Totals"}), "t.csv", STOPPED),
        ("share.exportTable", json!({"table": "Totals"}), "t.geojson", STOPPED),
        ("share.exportLayer", json!({"layer": "Places"}), "z.geojson", STOPPED),
        ("share.exportLayer", json!({"layer": "Places"}), "z.csv", STOPPED),
        ("share.exportLayer", json!({"layer": "Places"}), "z.parquet", STOPPED),
        ("share.exportLayer", json!({"layer": "Places"}), "z.kml", STOPPED),
        ("share.exportLayer", json!({"layer": "Places"}), "z.kmz", STOPPED),
        ("share.exportLayer", json!({"layer": "Places"}), "z.gpx", STOPPED),
        ("share.exportLayer", json!({"layer": "Places"}), "z.zip", STOPPED),
        ("data.convert", json!({"layer": "Places", "format": "geojson"}), "z.geojson", STOPPED),
        ("data.convert", json!({"layer": "Places", "format": "csv"}), "z.csv", STOPPED),
        ("data.convert", json!({"layer": "Places", "format": "kml"}), "z.kml", STOPPED),
        ("data.convert", json!({"layer": "Places", "format": "gpx"}), "z.gpx", STOPPED),
        ("data.convert", json!({"layer": "Places", "format": "wkt"}), "z.txt", STOPPED),
        ("share.exportRaster", json!({"layer": "Dem"}), "dem.tif", STOPPED),
        ("share.exportRaster", json!({"layer": "Dem", "cog": true}), "cog.tif", STOPPED),
        ("share.exportMap", json!({"width": 64, "height": 64}), "map.tif", STOPPED),
        ("share.exportMap", json!({"width": 64, "height": 64, "cog": true}), "mapcog.tif", STOPPED),
        ("layout.export", json!({"layout": "Page", "dpi": 72}), "page.pdf", STOPPED),
        ("report.export", json!({"report": "List", "dpi": 72}), "list.pdf", STOPPED),
        ("share.exportMap", json!({"width": 64, "height": 64}), "map.png", MADE),
        ("layout.export", json!({"layout": "Page", "dpi": 72}), "page.png", MADE),
        ("layout.export", json!({"layout": "Page", "dpi": 72}), "page.svg", MADE),
        ("report.export", json!({"report": "List", "dpi": 72}), "list.png", MADE),
        ("share.exportRaster", json!({"layer": "Made"}), "made.tif", MADE),
    ];
    for (command, ..) in crate::cmd::exports::IN_BACKGROUND {
        assert!(cases.iter().any(|c| c.0 == *command) || REFUSED.iter().any(|r| r.0 == *command), "{command}");
    }
    let files = Files::new();
    let mut s = session(&files, true);
    let (r, tif) = raster("Made", 1);
    s.map_mut().add_raster("Made", rustgis_map::RasterLayer::from_written(r, "made.tif", tif));
    for (command, params, file, refusal) in cases {
        s.download_cap = Some(10);
        let e = s.run(command, &to(&params, file)).unwrap_err();
        assert!(e.contains(file) && e.contains(refusal) && e.contains("export less"), "{command} {file}: {e}");
        assert!(e.contains("desktop app"), "{e}");
        assert!(s.take_gp_files().is_empty(), "{command} {file}");
        s.download_cap = Some(1 << 20);
        let pending = s.run(command, &to(&params, file)).unwrap_or_else(|e| panic!("{command} {file}: {e}"));
        let (_, bytes, answer) = deliver(&mut s, &pending, None);
        assert!(!bytes.is_empty() && bytes.len() <= 1 << 20, "{command} {file}: {}", bytes.len());
        answer.unwrap();
    }
}

/// The limit is what the downloads waiting leave of it: an export that would fit by itself is
/// refused, saying what is waiting, and a full queue is refused before anything is made.
#[test]
fn a_browser_export_is_held_to_what_the_waiting_downloads_leave() {
    let files = Files::new();
    let mut s = session(&files, true);
    let table = |path: &str| json!({"table": "Totals", "path": path});
    s.run("share.exportTable", &table("first.csv")).unwrap();
    let first = s.file_outputs.iter().filter_map(|f| f.bytes.as_ref()).map(|b| b.len()).sum::<usize>();
    s.download_cap = Some(first * 2 - 1);
    let e = s.run("share.exportTable", &table("second.csv")).unwrap_err();
    assert!(e.contains("second.csv: stopped once the file passed") && e.contains("already waiting"), "{e}");
    assert!(e.contains("split the task"), "{e}");
    s.download_cap = Some(first * 2);
    s.run("share.exportTable", &table("second.csv")).unwrap();
    let e = s.run("share.exportTable", &table("third.csv")).unwrap_err();
    assert!(e.contains("file export queue limit reached"), "{e}");
    assert_eq!(s.take_gp_files().len(), 2);
}

/// A project a command replaces keeps its downloads waiting until the command ends, in case it
/// is given back, and they count against the limit: an export in the new project can't pass it.
#[test]
fn downloads_put_aside_with_a_replaced_project_count_against_the_limit() {
    let files = Files::new();
    let mut s = session(&files, true);
    let script = "project.new\nshare.exportMap {\"width\": 64, \"height\": 64, \"path\": \"later.png\"}";
    // What the new project's map comes to by itself.
    let mut alone = session(&files, true);
    alone.run("script.run", &json!({"script": script})).unwrap();
    let map = alone.take_gp_files().iter().map(|f| f.2.len()).sum::<usize>();
    s.run("share.exportTable", &json!({"table": "Totals", "path": "totals.csv"})).unwrap();
    // Room for either file, not both.
    s.download_cap = Some(s.waiting_download_bytes() + map - 1);
    let e = s.run("script.run", &json!({"script": script})).unwrap_err();
    assert!(e.contains("share.exportMap"), "{e}");
    let waiting: Vec<String> = s.take_gp_files().into_iter().map(|f| f.1).collect();
    assert_eq!(waiting, ["totals.csv"]);
}

/// An export queued in a project a command opened, before that command fails, is said to be
/// dropped; one that waited in the project given back still waits.
#[test]
fn a_failed_commands_export_from_a_project_it_opened_is_said_dropped() {
    let files = Files::new();
    let mut s = session(&files, true);
    s.run("share.exportTable", &json!({"table": "Totals", "path": "totals.csv"})).unwrap();
    let mark = s.notice_mark();
    let script = "project.new\nshare.exportMap {\"width\": 64, \"height\": 64, \"path\": \"later.png\"}\nno.suchCommand";
    s.run("script.run", &json!({"script": script})).unwrap_err();
    let dropped: Vec<String> = said(&s, mark).into_iter().filter(|n| n.contains("not downloaded")).collect();
    assert_eq!(dropped, ["1 export(s) not downloaded, as the command that made them failed: later.png"]);
    let waiting: Vec<String> = s.take_gp_files().into_iter().map(|f| f.1).collect();
    assert_eq!(waiting, ["totals.csv"]);
}

/// A download the browser refuses is an error, and nothing says the file was exported.
#[test]
fn a_browser_export_not_taken_is_not_said_to_be_exported() {
    let files = Files::new();
    let mut s = session(&files, true);
    let pending = s.run("share.exportLayer", &json!({"layer": "Places", "path": "z.geojson"})).unwrap();
    let mark = s.notice_mark();
    let (_, _, answer) = deliver(&mut s, &pending, Some("download blocked"));
    assert_eq!(answer.unwrap()["delivered"], false);
    let notices = s.notices_since(mark);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].error && notices[0].text == "download blocked", "{notices:?}");
    assert!(s.project.gp_history.is_empty());
}

/// A queued download dropped by a later failure, or by another project replacing its own, is
/// said, an export's and a tool output's each in a notice of its own: the desktop would have
/// written the file by then, and a tool's run is recorded only once its file is delivered. A run
/// that keeps them says nothing.
#[test]
fn a_browser_download_dropped_before_it_is_offered_is_said() {
    let export = "share.exportTable {\"table\": \"Totals\", \"path\": \"totals.csv\"}";
    let tool = "gp.run {\"tool\": \"rasterToPmtiles\", \"params\": {\"input\": \"Dem\", \"minZoom\": 0, \"maxZoom\": 0}}";
    let expected = |queued: &[&str], why: &str| {
        let mut notices = Vec::new();
        if queued.contains(&export) {
            notices.push(format!("1 export(s) not downloaded, {why}: totals.csv"));
        }
        if queued.contains(&tool) {
            notices.push(format!(
                "1 tool output file(s) not downloaded, {why}, so their runs aren't in the geoprocessing history: Dem.pmtiles"
            ));
        }
        notices
    };
    let dropped = |s: &Session, mark: u64| -> Vec<String> { said(s, mark).into_iter().filter(|n| n.contains("not downloaded")).collect() };
    for queued in [&[export][..], &[tool], &[export, tool]] {
        for (then, why) in [
            ("map.zoomFull", None),
            ("no.suchCommand", Some("as the command that made them failed")),
            ("project.new", Some("as another project replaced theirs first")),
        ] {
            let files = Files::new();
            let mut s = session(&files, true);
            let mark = s.notice_mark();
            let ran = s.run("script.run", &json!({"script": format!("{}\n{then}", queued.join("\n"))}));
            assert_eq!(ran.is_ok(), then != "no.suchCommand", "{ran:?}");
            let notices = why.map(|why| expected(queued, why)).unwrap_or_default();
            assert_eq!(dropped(&s, mark), notices, "{queued:?}, {then}");
            assert_eq!(s.take_gp_files().len(), if why.is_none() { queued.len() } else { 0 }, "{queued:?}, {then}");
            assert!(s.project.gp_history.is_empty(), "no run is recorded before delivery");
        }
        // Replaced by itself, between the command and the frame that would offer the file.
        let files = Files::new();
        let mut s = session(&files, true);
        s.run("script.run", &json!({"script": queued.join("\n")})).unwrap();
        let mark = s.notice_mark();
        s.run("project.new", &json!({})).unwrap();
        assert_eq!(dropped(&s, mark), expected(queued, "as another project replaced theirs first"), "{queued:?}");
    }
}
