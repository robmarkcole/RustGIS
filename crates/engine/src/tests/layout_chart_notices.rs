//! A layout chart's notices on export, and in a box too small to draw it (#3156).

use super::*;
use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value as Data};

const COLUMNS: &str = "the chart's labels and values leave its columns no height: it draws as a placeholder until its box is taller";
const THIN: &str = "the chart is too small for its bars: some draw under 1 pt until its box is larger";

/// A layout "Charts" over a map with the points layer "Shops": `region` (E, N, S), `kind` and `n`.
fn shops() -> Session {
    let mut s = Session::new(sample::world_project());
    let mut ds = Dataset::new(
        "Shops",
        GeometryType::Point,
        vec![Field::new("region", FieldType::Text), Field::new("kind", FieldType::Text), Field::new("n", FieldType::Double)],
    );
    for (region, kind) in [("E", "city"), ("E", "town"), ("N", "town"), ("S", "town")] {
        ds.push(
            Some(Geometry::Point(rustgis_data::Coord::new(0.0, 0.0))),
            vec![Data::Text(region.into()), Data::Text(kind.into()), Data::Float(1.0)],
        );
    }
    s.map_mut().add_dataset(ds);
    s.run("layout.new", &json!({"name": "Charts"})).unwrap();
    s
}

/// Adds a chart of Shops summed by region, with `extra` keys; its element id.
fn chart(s: &mut Session, extra: Value) -> u64 {
    let mut p = json!({"kind": "chart", "layer": "Shops", "field": "n", "group": "region"});
    if let (Some(p), Some(e)) = (p.as_object_mut(), extra.as_object()) {
        p.extend(e.clone());
    }
    s.run("layout.addElement", &p).unwrap()["element"].as_u64().unwrap()
}

/// What running `command` with `p` says.
fn said(s: &mut Session, command: &str, p: Value) -> Vec<String> {
    let mark = s.notice_mark();
    s.run(command, &p).unwrap();
    s.notices_since(mark).iter().map(|n| n.text.clone()).collect()
}

/// What exporting the layout to a file of `ext` says of its charts.
fn exported(s: &mut Session, ext: &str) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("rustgis-3156-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("page.{ext}"));
    let mut all = said(s, "layout.export", json!({"path": path, "dpi": 36}));
    std::fs::remove_dir_all(&dir).unwrap();
    all.retain(|t| t.contains("chart"));
    all
}

/// What the export says of the chart `id`'s `note`.
fn of(s: &Session, id: u64, note: &str) -> String {
    let name = &s.project.layouts[0].element(id as u32).unwrap().name;
    format!("Chart \"{name}\" ({id}): {note}")
}

#[test]
fn an_export_says_what_each_chart_leaves_out() {
    let mut s = shops();
    let id = chart(&mut s, json!({"bars": 2}));
    let want =
        of(&s, id, "the chart draws 2 of 3 bars: 1 left out (\"bars\" draws up to 100, \"other\": true gathers the rest in an Other bar)");
    for ext in ["png", "pdf", "svg"] {
        assert_eq!(exported(&mut s, ext), std::slice::from_ref(&want), "{ext}");
    }
    // A project opened again says it too: nothing is kept from adding the chart.
    let mut again = Session::new(serde_json::from_value(serde_json::to_value(&s.project).unwrap()).unwrap());
    assert_eq!(exported(&mut again, "png"), [want]);
}

#[test]
fn an_export_reads_the_layer_as_it_is_now() {
    let mut s = shops();
    let id = chart(&mut s, json!({}));
    assert!(exported(&mut s, "png").is_empty(), "nothing lost, nothing said");
    let layer = s.map_mut().layers.iter_mut().find(|l| l.name == "Shops").unwrap();
    layer.dataset.push(
        Some(Geometry::Point(rustgis_data::Coord::new(0.0, 0.0))),
        vec![Data::Text("N".into()), Data::Text("town".into()), Data::Null],
    );
    let want = of(&s, id, "1 feature of Shops with no number in n left out of the chart");
    assert_eq!(exported(&mut s, "png"), [want]);
}

#[test]
fn a_map_series_export_says_a_pages_losses_once() {
    let mut s = shops();
    s.run("layout.series", &json!({"layer": "Shops", "nameField": "region"})).unwrap();
    // On page 2 alone, East's city and town take different colours.
    s.map_mut().layers.iter_mut().find(|l| l.name == "Shops").unwrap().symbol_by.fill =
        Some("CASE WHEN @page_number = 2 AND kind = 'city' THEN '#ff0000' ELSE '#0000ff' END".into());
    let id = chart(&mut s, json!({"classes": true}));
    let want = of(&s, id, "1 bar keeps the chart's bar colour: its features have several colours on the map");
    assert!(exported(&mut s, "png").is_empty(), "page 1 has one colour");
    assert_eq!(exported(&mut s, "pdf"), [want]);
    // The same loss on every page is said once.
    s.run("layout.update", &json!({"element": id, "classes": false, "bars": 2})).unwrap();
    let said = exported(&mut s, "pdf");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("draws 2 of 3 bars"), "{said:?}");
}

#[test]
fn an_export_names_charts_with_the_same_notice_in_one() {
    let mut s = shops();
    let ids: Vec<_> = (0..12).map(|_| chart(&mut s, json!({"bars": 2}))).collect();
    let other = chart(&mut s, json!({"bars": 1}));
    let said = exported(&mut s, "png");
    assert_eq!(said.len(), 2, "{said:?}");
    let named = |id: u64| format!("\"{}\" ({id})", s.project.layouts[0].element(id as u32).unwrap().name);
    let listed: Vec<_> = ids[..10].iter().map(|id| named(*id)).collect();
    let want = format!("12 charts ({} and 2 more): the chart draws 2 of 3 bars", listed.join(", "));
    assert!(said[0].starts_with(&want), "{said:?}");
    assert!(said[1].starts_with(&format!("Chart {}: the chart draws 1 of 3 bars", named(other))), "{said:?}");
}

#[test]
fn an_export_says_nothing_of_a_chart_it_does_not_draw() {
    let mut s = shops();
    let id = chart(&mut s, json!({"bars": 2}));
    s.run("layout.update", &json!({"element": id, "visible": false})).unwrap();
    assert!(exported(&mut s, "png").is_empty(), "hidden");
    s.run("layout.update", &json!({"element": id, "visible": true})).unwrap();
    s.run("layer.remove", &json!({"layer": "Shops"})).unwrap();
    assert!(exported(&mut s, "png").is_empty(), "its layer is gone");
}

/// The assistant's view of a layout says what exporting it says, of the charts in the region it
/// shows alone when it shows one.
#[test]
fn a_view_of_the_layout_says_the_notices_of_the_charts_it_shows() {
    let mut s = shops();
    let id = chart(&mut s, json!({"bars": 2, "x": 10, "y": 10, "w": 60, "h": 40}));
    let mut viewed = |region| {
        let mark = s.notice_mark();
        crate::cmd::layout::layout_png(&mut s, 0, 36.0, region).unwrap();
        s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains("chart")).collect::<Vec<_>>()
    };
    for region in [None, Some([0.0, 0.0, 40.0, 30.0])] {
        let said = viewed(region);
        assert!(said.len() == 1 && said[0].contains(&format!("({id}): the chart draws 2 of 3 bars")), "{region:?}: {said:?}");
    }
    assert!(viewed(Some([150.0, 100.0, 40.0, 30.0])).is_empty(), "a region without it");
}

#[test]
fn a_chart_of_columns_without_height_says_so() {
    let mut s = shops();
    // 10 mm high: its title, a line of labels and one of values leave the columns nothing.
    let small = 10.0;
    let mark = s.notice_mark();
    let id = chart(&mut s, json!({"orientation": "vertical", "values": true, "h": small}));
    let added: Vec<_> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(added.iter().any(|t| t == COLUMNS), "added: {added:?}");
    let want = of(&s, id, COLUMNS);
    assert_eq!(exported(&mut s, "png"), [want]);
    let taller = said(&mut s, "layout.update", json!({"element": id, "h": 60}));
    assert!(!taller.iter().any(|t| t == COLUMNS), "{taller:?}");
    let resized = said(&mut s, "layout.update", json!({"element": id, "h": small}));
    assert!(resized.iter().any(|t| t == COLUMNS), "resized: {resized:?}");
    let moved = said(&mut s, "layout.update", json!({"element": id, "x": 20}));
    assert!(!moved.iter().any(|t| t == COLUMNS), "moved: {moved:?}");
}

#[test]
fn a_chart_with_bars_under_a_point_says_so() {
    let mut s = shops();
    let id = chart(&mut s, json!({"h": 60, "w": 80}));
    assert!(exported(&mut s, "png").is_empty(), "room enough");
    for (key, value) in [("h", 5.0), ("w", 1.0)] {
        s.run("layout.update", &json!({"element": id, "h": 60, "w": 80})).unwrap();
        let resized = said(&mut s, "layout.update", json!({"element": id, key: value}));
        assert!(resized.iter().any(|t| t == THIN), "{key}: {resized:?}");
        let want = of(&s, id, THIN);
        assert_eq!(exported(&mut s, "svg"), [want], "{key}");
    }
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

/// What exporting the report "R" to a file of `ext` says of charts.
fn reported(s: &mut Session, ext: &str) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("rustgis-3166-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = Dir(dir);
    let path = dir.0.join(format!("report.{ext}"));
    let mut all = said(s, "report.export", json!({"report": "R", "path": path, "dpi": 36}));
    all.retain(|t| t.contains("chart"));
    all
}

/// What a report says of the chart `id` of the layout `li`'s `note`.
fn of_page(s: &Session, li: usize, id: u64, note: &str) -> String {
    let layout = &s.project.layouts[li];
    let name = &layout.element(id as u32).unwrap().name;
    format!("Chart \"{name}\" ({id}) in \"{}\": {note}", layout.name)
}

const TWO_OF_THREE: &str =
    "the chart draws 2 of 3 bars: 1 left out (\"bars\" draws up to 100, \"other\": true gathers the rest in an Other bar)";

#[test]
fn a_report_says_what_the_charts_of_its_layout_pages_leave_out() {
    let mut s = shops();
    let id = chart(&mut s, json!({"bars": 2}));
    // The same layout twice: said once.
    report(&mut s, &["Charts", "Charts"]);
    let want = of_page(&s, 0, id, TWO_OF_THREE);
    assert_eq!(reported(&mut s, "pdf"), std::slice::from_ref(&want));
    // A project opened again says it too.
    let mut again = Session::new(serde_json::from_value(serde_json::to_value(&s.project).unwrap()).unwrap());
    assert_eq!(reported(&mut again, "pdf"), std::slice::from_ref(&want));
    // The layout's own export names no layout.
    assert_eq!(exported(&mut s, "pdf"), [of(&s, id, TWO_OF_THREE)]);
}

#[test]
fn a_report_image_says_the_charts_of_its_first_page_alone() {
    let mut s = shops();
    let id = chart(&mut s, json!({"bars": 2}));
    report(&mut s, &["Charts"]);
    assert!(reported(&mut s, "png").is_empty(), "the layout is not on the first page");
    s.run("report.existingTemplate", &json!({"report": "R", "layout": "Charts"})).unwrap();
    assert_eq!(reported(&mut s, "png"), [of_page(&s, 0, id, TWO_OF_THREE)]);
}

#[test]
fn a_report_names_charts_of_two_layouts_with_the_same_notice_in_one() {
    let mut s = shops();
    let first = chart(&mut s, json!({"bars": 2}));
    s.run("layout.new", &json!({"name": "More"})).unwrap();
    let second = chart(&mut s, json!({"layout": 1, "bars": 2}));
    assert_eq!(first, second, "each layout numbers its own elements");
    report(&mut s, &["Charts", "More"]);
    let named = |li: usize| {
        let name = &s.project.layouts[li].element(first as u32).unwrap().name;
        format!("\"{name}\" ({first}) in \"{}\"", s.project.layouts[li].name)
    };
    let want = format!("2 charts ({}, {}): {TWO_OF_THREE}", named(0), named(1));
    assert_eq!(reported(&mut s, "pdf"), [want]);
}

#[test]
fn a_report_says_a_chart_box_too_small_for_it() {
    let mut s = shops();
    let id = chart(&mut s, json!({"orientation": "vertical", "values": true, "h": 10}));
    report(&mut s, &["Charts"]);
    assert_eq!(reported(&mut s, "pdf"), [of_page(&s, 0, id, COLUMNS)]);
}

#[test]
fn a_report_says_nothing_of_charts_with_no_losses_or_with_no_layout_page() {
    let mut s = shops();
    let id = chart(&mut s, json!({}));
    report(&mut s, &[]);
    assert!(reported(&mut s, "pdf").is_empty(), "no layout page");
    s.run("report.existingTemplate", &json!({"report": "R", "layout": "Charts"})).unwrap();
    assert!(reported(&mut s, "pdf").is_empty(), "nothing lost");
    s.run("layout.update", &json!({"element": id, "bars": 2, "visible": false})).unwrap();
    assert!(reported(&mut s, "pdf").is_empty(), "hidden");
}
