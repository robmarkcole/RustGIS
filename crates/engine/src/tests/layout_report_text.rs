//! A report says the texts cut off on the layouts it draws as pages (#3195).

use super::*;
use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value as Data};

/// A map with the points layer "Shops" and the layout "Sheet".
fn sheet() -> Session {
    let mut s = Session::new(sample::world_project());
    let mut ds = Dataset::new("Shops", GeometryType::Point, vec![Field::new("n", FieldType::Double)]);
    ds.push(Some(Geometry::Point(rustgis_data::Coord::new(0.0, 0.0))), vec![Data::Float(1.0)]);
    s.map_mut().add_dataset(ds);
    s.run("layout.new", &json!({"name": "Sheet"})).unwrap();
    s
}

/// Adds the text `text` to the layout `li`, in a box `w` by `h` millimetres; its element id.
fn text(s: &mut Session, li: usize, text: &str, w: f64, h: f64) -> u64 {
    let p = json!({"layout": li, "kind": "text", "text": text, "x": 20, "y": 20, "w": w, "h": h});
    s.run("layout.addElement", &p).unwrap()["element"].as_u64().unwrap()
}

/// Adds a text of three lines to the layout `li`, in a box that holds one.
fn cut(s: &mut Session, li: usize) -> u64 {
    text(s, li, "one\ntwo\nthree", 60.0, 5.0)
}

/// A report "R" over Shops that draws each of `layouts` as a page after its own pages.
fn report(s: &mut Session, layouts: &[&str]) {
    s.run("report.new", &json!({"layer": "Shops", "name": "R"})).unwrap();
    for layout in layouts {
        s.run("report.existingTemplate", &json!({"report": "R", "layout": layout, "end": true})).unwrap();
    }
}

/// Removed however the test ends.
struct Dir(std::path::PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// What exporting the report "R" to a file of `ext` says of cut-off text.
fn reported(s: &mut Session, ext: &str) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("rustgis-3195-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = Dir(dir);
    let path = dir.0.join(format!("report.{ext}"));
    let mark = s.notice_mark();
    s.run("report.export", &json!({"report": "R", "path": path, "dpi": 72})).unwrap();
    s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains("cut off")).collect()
}

/// What a report says of the one text `id` cut off on the layout `li`.
fn of_page(s: &Session, li: usize, id: u64) -> String {
    let layout = &s.project.layouts[li];
    let note = rustgis_render::layout::clipped_text_notice(layout, &[(id as u32, layout.element(id as u32).unwrap().name.clone())]);
    format!("The report, layout \"{}\": {}", layout.name, note.unwrap())
}

#[test]
fn a_report_says_the_text_cut_off_on_its_layout_pages() {
    let mut s = sheet();
    let id = cut(&mut s, 0);
    // The same layout twice: said once, the text counted once.
    report(&mut s, &["Sheet", "Sheet"]);
    let want = of_page(&s, 0, id);
    assert!(want.contains("Text is cut off in 1 layout element(s)"), "{want}");
    assert_eq!(reported(&mut s, "pdf"), [want]);
}

#[test]
fn a_report_names_each_layout_with_cut_off_text() {
    let mut s = sheet();
    let first = cut(&mut s, 0);
    s.run("layout.new", &json!({"name": "More"})).unwrap();
    let second = cut(&mut s, 1);
    assert_eq!(first, second, "each layout numbers its own elements");
    report(&mut s, &["Sheet", "More"]);
    let want = [of_page(&s, 0, first), of_page(&s, 1, second)];
    assert_eq!(reported(&mut s, "pdf"), want);
}

#[test]
fn a_report_says_nothing_of_text_that_fits() {
    let mut s = sheet();
    text(&mut s, 0, "one", 60.0, 20.0);
    report(&mut s, &["Sheet"]);
    assert!(reported(&mut s, "pdf").is_empty());
}

#[test]
fn a_report_image_says_the_text_of_its_first_page_alone() {
    let mut s = sheet();
    let id = cut(&mut s, 0);
    report(&mut s, &["Sheet"]);
    assert!(reported(&mut s, "png").is_empty(), "the layout is not on the first page");
    s.run("report.existingTemplate", &json!({"report": "R", "layout": "Sheet"})).unwrap();
    assert_eq!(reported(&mut s, "png"), [of_page(&s, 0, id)]);
}

#[test]
fn a_report_measures_each_layout_page_with_its_own_page_number() {
    let mut s = sheet();
    // One line while the page number has one digit; two from page 10.
    let id = text(&mut s, 0, &["{page}"; 8].join(" "), 18.0, 6.0);
    s.run("layout.update", &json!({"element": id, "wrap": true, "size": 10})).unwrap();
    report(&mut s, &["Sheet"]);
    assert!(reported(&mut s, "pdf").is_empty(), "a one-digit page fits");
    for _ in 0..9 {
        s.run("report.existingTemplate", &json!({"report": "R", "layout": "Sheet", "end": true})).unwrap();
    }
    assert_eq!(reported(&mut s, "pdf"), [of_page(&s, 0, id)]);
}
