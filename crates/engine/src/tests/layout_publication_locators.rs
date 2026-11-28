//! A publication's inset and globe locators together (#3240): the inset outlines the map frame and
//! the globe outlines the inset.

use super::*;
use rustgis_map::layout::{Element, ElementKind, FrameShape};

fn publish(s: &mut Session, parts: Value) -> Result<Value, String> {
    let mut p = json!({"profile": "a4-portrait", "extent": [-46.8, -11.6, -38.5, -2.2]});
    p.as_object_mut().unwrap().extend(parts.as_object().unwrap().clone());
    s.run("layout.publication", &p)
}

fn sheet(parts: Value) -> (Session, Value) {
    let mut s = world();
    let r = publish(&mut s, parts).unwrap();
    (s, r)
}

/// Open sea: with nothing to cover, each surround stays in its home corner.
const SEA: [f64; 4] = [-30.0, -30.0, -20.0, -22.0];

fn find<'a>(s: &'a Session, name: &str) -> &'a Element {
    let elements = &s.project.layouts.last().unwrap().elements;
    elements.iter().find(|e| e.name == name).unwrap_or_else(|| panic!("no {name}"))
}

fn has(s: &Session, name: &str) -> bool {
    s.project.layouts.last().unwrap().elements.iter().any(|e| e.name == name)
}

/// A frame element's map, shape and the frame it outlines.
fn frame_of(e: &Element) -> (&str, &FrameShape, Option<u32>) {
    match &e.kind {
        ElementKind::MapFrame { map, shape, extent_of, .. } => (map, shape, *extent_of),
        k => panic!("{k:?}"),
    }
}

fn apart(a: &Element, b: &Element) -> bool {
    a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y
}

fn elements(s: &Session) -> Value {
    serde_json::to_value(&s.project.layouts.last().unwrap().elements).unwrap()
}

#[test]
fn an_inset_and_a_globe_nest_the_globe_outlining_the_inset() {
    let (s, r) = sheet(json!({"extent": SEA, "locator": ["inset", "globe"]}));
    let main = s.map().name.clone();
    let globe_map = format!("{main} Locator");
    assert_eq!(r["locator"], json!([main, globe_map]), "{r}");
    let (map, inset, globe) = (find(&s, "Map Frame"), find(&s, "Locator Inset"), find(&s, "Locator Globe"));
    assert_eq!(frame_of(inset), (main.as_str(), &FrameShape::Rectangle, Some(map.id)));
    assert_eq!(frame_of(globe), (globe_map.as_str(), &FrameShape::Circle, Some(inset.id)));
    assert!(!has(&s, "Locator"));
    // Side by side in the locator's corner, the inset against the frame's edge.
    assert!(apart(inset, globe) && (inset.y - globe.y).abs() < 1e-6 && (inset.w - globe.w).abs() < 1e-6);
    assert!(inset.x < globe.x && inset.x > map.x && inset.x - map.x < inset.w, "{inset:?} {globe:?}");
    assert!(apart(find(&s, "Legend Box"), inset) && apart(find(&s, "Legend Box"), globe));
}

#[test]
fn the_order_of_two_locators_changes_nothing() {
    let (a, ra) = sheet(json!({"locator": ["inset", "globe"]}));
    let (b, rb) = sheet(json!({"locator": ["globe", "inset"]}));
    assert_eq!((elements(&a), &ra["locator"]), (elements(&b), &rb["locator"]));
}

#[test]
fn a_list_of_one_locator_is_its_name() {
    for one in ["globe", "inset"] {
        let (a, ra) = sheet(json!({"locator": [one]}));
        let (b, rb) = sheet(json!({"locator": one}));
        assert_eq!((elements(&a), &ra["locator"]), (elements(&b), &rb["locator"]), "{one}");
        assert!(has(&a, "Locator") && ra["locator"].is_string(), "{one}: {ra}");
    }
    let (a, ra) = sheet(json!({"locator": []}));
    let (b, _) = sheet(json!({"locator": "none"}));
    assert_eq!(elements(&a), elements(&b));
    assert!(ra["locator"].is_null() && !has(&a, "Locator"), "{ra}");
}

#[test]
fn a_locator_list_with_a_wrong_entry_is_refused_and_adds_nothing() {
    let mut s = world();
    let before = (s.project.maps.len(), s.project.layouts.len());
    for (list, why) in [
        (json!(["globe", "globe"]), "\"globe\" is given twice"),
        (json!(["inset", "globe", "inset"]), "\"inset\" is given twice"),
        (json!(["inset", "none"]), "\"none\" goes alone"),
        (json!(["none"]), "\"none\" goes alone"),
        (json!(["mosaic"]), "locator is"),
        (json!([3]), "locator is"),
    ] {
        let e = publish(&mut s, json!({"locator": list})).unwrap_err();
        assert!(e.contains(why), "{list}: {e}");
    }
    assert_eq!((s.project.maps.len(), s.project.layouts.len()), before);
}

#[test]
fn a_figure_and_a_whole_globe_draw_both_locators_asked_for() {
    let (s, _) = sheet(json!({"profile": "figure-2col", "locator": ["inset", "globe"]}));
    assert!(has(&s, "Locator Inset") && has(&s, "Locator Globe"));
    let mut s = world();
    s.map_mut().set_projection(rustgis_map::Projection::Globe);
    s.run("layout.publication", &json!({"profile": "a4-portrait", "locator": ["inset", "globe"]})).unwrap();
    assert!(has(&s, "Locator Inset") && has(&s, "Locator Globe"));
}

#[test]
fn a_column_style_holds_both_locators_at_its_top() {
    for style in ["panel", "modular"] {
        let (s, _) = sheet(json!({"profile": "a4-landscape", "style": style, "locator": ["inset", "globe"]}));
        let (map, inset, globe) = (find(&s, "Map Frame"), find(&s, "Locator Inset"), find(&s, "Locator Globe"));
        assert!(inset.x >= map.x + map.w && globe.x >= map.x + map.w, "{style}: in the column");
        assert!(apart(inset, globe) && (inset.y - globe.y).abs() < 1e-6 && inset.x < globe.x, "{style}");
        let legend = find(&s, "Legend Box");
        assert!(legend.y >= inset.y + inset.h, "{style}: the legend under them");
    }
}

#[test]
fn both_locators_take_the_arranged_corner() {
    let corners = json!({"locator": "bottom-right", "northArrow": "top-left", "legend": "top-right", "scaleBar": "bottom-left"});
    let (s, _) = sheet(json!({"extent": SEA, "locator": ["inset", "globe"], "arrangement": corners}));
    let (map, inset, globe) = (find(&s, "Map Frame"), find(&s, "Locator Inset"), find(&s, "Locator Globe"));
    let my = map.y + map.h / 2.0;
    assert!(inset.y > my && globe.y > my && map.x + map.w - (inset.x + inset.w) < inset.w, "{inset:?} {globe:?}");
    assert!(inset.x > globe.x, "the inset stays against the frame's edge");
}

#[test]
fn two_locators_reuse_an_orphan_globe_and_undo_together() {
    let mut s = world();
    let maps = s.project.maps.len();
    let first = publish(&mut s, json!({"locator": ["inset", "globe"], "name": "Again"})).unwrap()["locator"][1].clone();
    s.run("layout.remove", &json!({"name": "Again"})).unwrap();
    let r = publish(&mut s, json!({"locator": ["inset", "globe"], "name": "Again"})).unwrap();
    assert_eq!((s.project.maps.len(), &r["locator"][1]), (maps + 1, &first), "{r}");
    let layouts = s.project.layouts.len();
    publish(&mut s, json!({"locator": ["inset", "globe"]})).unwrap();
    assert_eq!(s.project.maps.len(), maps + 2);
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!((s.project.maps.len(), s.project.layouts.len()), (maps + 1, layouts));
}

#[test]
fn a_scale_box_measured_again_stays_off_either_locator() {
    for name in ["Locator Inset", "Locator Globe"] {
        let (mut s, _) = sheet(json!({"locator": ["inset", "globe"]}));
        let id = |s: &Session, n: &str| find(s, n).id;
        let (frame, text, boxed, locator) = (id(&s, "Map Frame"), id(&s, "Scale"), id(&s, "Scale Box"), id(&s, name));
        s.run("layout.update", &json!({"element": text, "text": "{scale}"})).unwrap();
        s.run("layout.update", &json!({"element": frame, "scale": 1_500_000.0})).unwrap();
        let (narrow, map) = (find(&s, "Scale Box").clone(), find(&s, "Map Frame").clone());
        // Beside the box on the side it grows to: the left one when it keeps its right edge.
        let x = if narrow.x + narrow.w / 2.0 > map.x + map.w / 2.0 { narrow.x - 5.5 } else { narrow.x + narrow.w + 0.5 };
        s.run("layout.update", &json!({"element": locator, "x": x, "y": narrow.y, "w": 5.0, "h": 5.0})).unwrap();
        let mark = s.notice_mark();
        s.run("layout.update", &json!({"element": text, "text": "Escala aproximada da folha {scale}"})).unwrap();
        s.run("layout.update", &json!({"element": frame, "scale": 30_000_000.0})).unwrap();
        let after = find(&s, "Scale Box");
        assert_eq!((after.id, after.x, after.w), (boxed, narrow.x, narrow.w), "{name}: the box stays");
        let said = format!("would now cover the {}", name.to_lowercase());
        assert!(s.notices_since(mark).iter().any(|n| n.text.contains(&said)), "{name}: said");
    }
}
