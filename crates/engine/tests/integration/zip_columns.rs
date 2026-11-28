//! The `x`, `y`, `wkt` and `delimiter` given when adding a zip reach its delimited and workbook
//! members (#3062), by path, as bytes and as a finished download.
use base64::Engine;
use rustgis_engine::data::{sheets, zip};
use rustgis_engine::{Project, Session};
use serde_json::{Value, json};

/// A zip of three tables that hold the columns `LongitudeOR`/`LatitudeOR` (which name detection
/// doesn't take) and one that doesn't.
fn members(with_notes: bool) -> Vec<u8> {
    let mut entries = vec![
        ("d/2023.csv".to_string(), b"id,LongitudeOR,LatitudeOR\n1,10,20\n2,11,21\n".to_vec()),
        ("d/2024.csv".to_string(), b"id;LongitudeOR;LatitudeOR\n3;12;22\n".to_vec()),
        ("book.xlsx".to_string(), sheets::xlsx(&[("ALL", &[&["id", "LongitudeOR", "LatitudeOR"], &["4", "13", "23"]])])),
    ];
    if with_notes {
        entries.push(("notes.csv".to_string(), b"id,name\n5,x\n".to_vec()));
    }
    zip::write(&entries).unwrap()
}

/// A folder of its own for each read: the tests run side by side.
fn unique_dir() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "rustgis-zip-columns-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// Adds `bytes` named `x.zip` by `route` with `extra` parameters; the session and its notices.
fn add(route: &str, bytes: &[u8], extra: Value) -> (Session, Vec<String>) {
    let dir = unique_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Session::new(Project::default());
    let mark = s.notice_mark();
    let mut p = extra;
    match route {
        "path" => {
            let path = dir.join("x.zip");
            std::fs::write(&path, bytes).unwrap();
            p["path"] = json!(path);
            s.run("layer.addData", &p).unwrap();
        }
        "bytes" => {
            p["fileName"] = json!("x.zip");
            p["data"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes));
            s.run("layer.addDataBytes", &p).unwrap();
        }
        _ => {
            let id = s.download_started();
            s.download_landed(id, bytes.to_vec());
            p["id"] = json!(id);
            p["url"] = json!("https://example.org/x.zip");
            s.run("layer.addDownload", &p).unwrap();
        }
    }
    let said = s.notices_since(mark).iter().map(|n| n.text.to_string()).collect();
    let _ = std::fs::remove_dir_all(&dir);
    (s, said)
}

fn columns() -> Value {
    json!({"x": "LongitudeOR", "y": "LatitudeOR"})
}

/// Each point layer's name and features' count, in name order, and the tables' names.
fn shape(s: &Session) -> (Vec<(String, usize)>, Vec<String>) {
    let mut layers: Vec<_> = s.map().layers.iter().map(|l| (l.dataset.name.clone(), l.dataset.features.len())).collect();
    layers.sort();
    let mut tables: Vec<_> = s.project.tables.iter().map(|t| t.dataset.name.clone()).collect();
    tables.sort();
    (layers, tables)
}

#[test]
fn a_zips_csv_and_workbook_members_take_the_columns_given() {
    for route in ["path", "bytes", "download"] {
        let (s, said) = add(route, &members(true), columns());
        let (layers, tables) = shape(&s);
        assert_eq!(layers.iter().map(|(_, n)| *n).collect::<Vec<_>>(), [2, 1, 1], "{route}: {layers:?}");
        assert_eq!(tables, ["notes"], "{route}");
        // One notice for the zip, with how many tables took the columns and the one that didn't.
        let lacking: Vec<_> = said.iter().filter(|t| t.contains("lacking a column named")).collect();
        assert_eq!(lacking.len(), 1, "{route}: {said:?}");
        assert!(
            lacking.iter().all(|t| t.contains(
                "the x, y or WKT columns given were used by 3 of 4 tables; the rest were read without them, lacking a column named: notes.csv (no x column 'LongitudeOR')"
            )),
            "{route}: {said:?}"
        );
    }
}

#[test]
fn a_zip_whose_members_all_have_the_columns_says_nothing_about_them() {
    let (s, said) = add("path", &members(false), columns());
    assert_eq!(shape(&s).0.len(), 3);
    assert!(!said.iter().any(|t| t.contains("lacking a column named")), "{said:?}");
    // Without the columns given, the zip reads as before: tables only, and nothing new said.
    let (s, said) = add("path", &members(true), json!({}));
    let (layers, tables) = shape(&s);
    assert!(layers.is_empty(), "{layers:?}");
    assert_eq!(tables.len(), 4, "{tables:?}");
    assert!(!said.iter().any(|t| t.contains("lacking a column named")), "{said:?}");
}

#[test]
fn a_zips_wkt_column_and_delimiter_reach_its_tables() {
    let entries =
        vec![("a.csv".to_string(), b"id,footprint\n1,POINT (1 2)\n".to_vec()), ("b.csv".to_string(), b"a,b\tc\n1,2\t3\n".to_vec())];
    let bytes = zip::write(&entries).unwrap();
    let (s, said) = add("path", &bytes, json!({"wkt": "footprint", "delimiter": "tab"}));
    // `a.csv` has no tab, so it is one column without `footprint`: read without the column given.
    let fields = |name: &str| {
        s.project
            .tables
            .iter()
            .find(|t| t.dataset.name == name)
            .map(|t| t.dataset.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>())
    };
    assert_eq!(fields("b"), Some(vec!["a,b".to_string(), "c".to_string()]), "{said:?}");
    // Read by commas, `a.csv`'s footprint becomes geometry.
    let (s, _) = add("path", &bytes, json!({"wkt": "footprint", "delimiter": ","}));
    assert!(s.map().layers.iter().any(|l| l.dataset.name == "a" && l.dataset.features.len() == 1));
}

#[test]
fn a_zips_other_members_ignore_the_columns_and_keep_their_notices() {
    let kml = br#"<?xml version="1.0"?><kml xmlns="http://www.opengis.net/kml/2.2"><Document><Placemark><name>p</name><Point><coordinates>1,2</coordinates></Point></Placemark><GroundOverlay><name>o</name><Icon><href>p.png</href></Icon><LatLonBox><north>1</north><south>0</south><east>1</east><west>0</west></LatLonBox></GroundOverlay></Document></kml>"#;
    let entries = vec![
        ("a.csv".to_string(), b"id,LongitudeOR,LatitudeOR\n1,10,20\n".to_vec()),
        ("o.kml".to_string(), kml.to_vec()),
        ("junk.txt".to_string(), b"just some prose".to_vec()),
    ];
    let bytes = zip::write(&entries).unwrap();
    let (s, said) = add("path", &bytes, columns());
    assert!(s.map().layers.iter().any(|l| l.dataset.name == "a" && l.dataset.features.len() == 1));
    assert!(s.map().layers.len() >= 2, "the KML's own layer stays");
    assert!(said.iter().any(|t| t.contains("skipped") && t.contains("junk.txt")), "{said:?}");
    assert!(said.iter().any(|t| t.contains("1 ground overlays (pictures) aren't drawn from inside a zip, in o.kml (1)")), "{said:?}");
    // Every notice but the columns' (there is none here) is what the zip says without the columns given.
    let (_, before) = add("path", &bytes, json!({}));
    let strip =
        |said: &[String]| said.iter().map(|t| t.rsplit_once(": ").map_or(t.clone(), |(_, rest)| rest.to_string())).collect::<Vec<_>>();
    assert_eq!(strip(&said), strip(&before));
}

#[test]
fn a_zips_picked_layer_keeps_the_zips_notices() {
    let mut entries = vec![("a.csv".to_string(), b"id,LongitudeOR,LatitudeOR\n1,10,20\n".to_vec())];
    entries.push(("junk.txt".to_string(), b"just some prose".to_vec()));
    let (s, said) = add("path", &zip::write(&entries).unwrap(), json!({"x": "LongitudeOR", "y": "LatitudeOR", "layer": "a"}));
    assert_eq!(shape(&s).0, [("a".to_string(), 1)]);
    assert!(said.iter().any(|t| t.contains("skipped") && t.contains("junk.txt")), "{said:?}");
}

#[test]
fn a_zip_given_x_without_y_or_both_kinds_of_geometry_is_refused_or_said() {
    let bytes = members(false);
    for p in [json!({"x": "LongitudeOR"}), json!({"x": "LongitudeOR", "y": "LatitudeOR", "wkt": "LongitudeOR"})] {
        let mut s = Session::new(Project::default());
        let dir = unique_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.zip");
        std::fs::write(&path, &bytes).unwrap();
        let mut q = p;
        q["path"] = json!(path);
        let ran = s.run("layer.addData", &q);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(ran.is_err(), "{ran:?}");
    }
}

#[test]
fn a_zips_columns_notice_goes_with_a_picked_layer_that_is_not_the_first() {
    let (s, said) = add("path", &members(true), json!({"x": "LongitudeOR", "y": "LatitudeOR", "layer": "2024"}));
    assert_eq!(shape(&s).0, [("2024".to_string(), 1)]);
    assert!(said.iter().any(|t| t.contains("used by 3 of 4 tables") && t.contains("notes.csv")), "{said:?}");
}
