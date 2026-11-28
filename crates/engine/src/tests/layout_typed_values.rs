//! A layout value of the wrong type is refused, not ignored (#3110, #3169).

use super::*;
use rustgis_map::layout::{Element, ElementKind, Grid, TickSide};

/// A layout with a map frame, a grid on it, a scale bar and a north arrow.
fn laid_out() -> Session {
    let mut s = world();
    s.run("layout.new", &json!({"template": "map"})).unwrap();
    for kind in ["grid", "scaleBar", "northArrow"] {
        s.run("layout.addElement", &json!({"kind": kind})).unwrap();
    }
    s
}

fn elements(s: &Session) -> &[Element] {
    &s.project.layouts[0].elements
}

/// The id of the last element of the kind `is` picks.
fn id(s: &Session, is: fn(&ElementKind) -> bool) -> u64 {
    elements(s).iter().rev().find(|e| is(&e.kind)).unwrap().id.into()
}

fn frame(s: &Session) -> u64 {
    id(s, |k| matches!(k, ElementKind::MapFrame { .. }))
}

fn grid(s: &Session) -> Grid {
    elements(s)
        .iter()
        .find_map(|e| match &e.kind {
            ElementKind::MapFrame { grid, .. } => grid.clone(),
            _ => None,
        })
        .unwrap()
}

/// Runs `command` with `params` and expects it refused naming `said`, with nothing changed and no
/// undo step.
fn refused(s: &mut Session, command: &str, params: Value, said: &str) {
    let before = serde_json::to_value(&s.project).unwrap();
    let undo = s.undo_label();
    let err = s.run(command, &params).expect_err(&params.to_string());
    assert!(err.contains(said), "{params}: {err}");
    assert_eq!(serde_json::to_value(&s.project).unwrap(), before, "{params}");
    assert_eq!(s.undo_label(), undo, "{params}");
}

fn wrong() -> [Value; 4] {
    [json!(17), json!(true), json!([]), json!({})]
}

#[test]
fn a_grid_name_of_the_wrong_type_is_refused() {
    let mut s = laid_out();
    let frame = frame(&s);
    for key in ["type", "style", "format", "labelOrientation"] {
        for v in wrong() {
            let mut add = json!({"kind": "grid"});
            add[key] = v.clone();
            refused(&mut s, "layout.addElement", add, &format!("{key} is "));
            let mut options = json!({});
            options[key] = v;
            refused(&mut s, "layout.update", json!({"element": frame, "grid": options}), &format!("{key} is "));
        }
    }
}

#[test]
fn a_grid_number_of_the_wrong_type_is_refused() {
    let mut s = laid_out();
    let frame = frame(&s);
    for key in ["step", "tickLength", "width"] {
        for v in [json!("bad"), json!(""), json!(true), json!([]), json!({})] {
            let mut add = json!({"kind": "grid"});
            add[key] = v.clone();
            refused(&mut s, "layout.addElement", add, &format!("{key} is "));
            let mut options = json!({});
            options[key] = v;
            refused(&mut s, "layout.update", json!({"element": frame, "grid": options}), &format!("{key} is "));
        }
    }
}

#[test]
fn a_scale_bar_or_north_arrow_value_of_the_wrong_type_is_refused() {
    let mut s = laid_out();
    let bar = id(&s, |k| matches!(k, ElementKind::ScaleBar { .. }));
    let arrow = id(&s, |k| matches!(k, ElementKind::NorthArrow { .. }));
    for v in wrong() {
        for (kind, element, key) in [("scaleBar", bar, "style"), ("scaleBar", bar, "units"), ("northArrow", arrow, "style")] {
            let mut add = json!({"kind": kind});
            add[key] = v.clone();
            refused(&mut s, "layout.addElement", add, &format!("{key} is "));
            let mut update = json!({"element": element});
            update[key] = v.clone();
            refused(&mut s, "layout.update", update, &format!("{key} is "));
        }
    }
}

#[test]
fn a_publication_part_value_of_the_wrong_type_is_refused_and_nothing_is_made() {
    for (part, key) in [("grid", "type"), ("grid", "step"), ("scaleBar", "units"), ("scaleBar", "style"), ("northArrow", "style")] {
        let mut s = world();
        let mut options = json!({});
        options[key] = json!([17]);
        let mut p = json!({"profile": "a4-portrait", "extent": [-30.0, -30.0, -20.0, -22.0]});
        p[part] = options;
        refused(&mut s, "layout.publication", p, &format!("{key} is "));
    }
}

#[test]
fn a_null_value_is_not_given() {
    let mut s = laid_out();
    let (frame, before) = (frame(&s), grid(&s));
    let nulls =
        json!({"type": null, "style": null, "format": null, "labelOrientation": null, "step": null, "tickLength": null, "width": null});
    s.run("layout.update", &json!({"element": frame, "grid": nulls})).unwrap();
    assert_eq!(grid(&s), before);
    let bar = id(&s, |k| matches!(k, ElementKind::ScaleBar { .. }));
    let kept = serde_json::to_value(elements(&s)).unwrap();
    s.run("layout.update", &json!({"element": bar, "style": null, "units": null, "divisions": null})).unwrap();
    assert_eq!(serde_json::to_value(elements(&s)).unwrap(), kept);
}

#[test]
fn a_number_given_as_text_and_a_known_name_are_read_as_before() {
    let mut s = laid_out();
    let frame = frame(&s);
    s.run("layout.update", &json!({"element": frame, "grid": {"step": " 5 ", "style": "ticks", "tickLength": "3"}})).unwrap();
    let g = grid(&s);
    assert_eq!((g.step, g.tick_length, g.style.code()), (5.0, 3.0, "ticks"));
    let mut s = laid_out();
    refused(&mut s, "layout.addElement", json!({"kind": "grid", "style": "wavy"}), "unknown grid style 'wavy'");
}

#[test]
fn a_colorbar_orientation_of_the_wrong_type_is_refused() {
    let mut s = laid_out();
    refused(&mut s, "layout.addElement", json!({"kind": "colorbar", "orientation": 17}), "orientation is ");
}

/// [`laid_out`], with a legend and a colorbar too.
fn with_keys() -> Session {
    let mut s = laid_out();
    for kind in ["legend", "colorbar"] {
        s.run("layout.addElement", &json!({"kind": kind})).unwrap();
    }
    s
}

#[test]
fn a_colorbar_or_legend_value_of_the_wrong_type_is_refused() {
    let mut s = with_keys();
    let bar = id(&s, |k| matches!(k, ElementKind::Colorbar { .. }));
    let legend = id(&s, |k| matches!(k, ElementKind::Legend { .. }));
    for v in wrong() {
        let keys = [("colorbar", bar, "tickSide"), ("colorbar", bar, "ticks"), ("colorbar", bar, "layer")];
        for (kind, element, key) in keys.into_iter().chain([("colorbar", bar, "title"), ("legend", legend, "title")]) {
            // A number is what ticks takes: text that isn't one stands in for it.
            let v = if key == "ticks" && v.is_number() { json!("many") } else { v.clone() };
            let mut add = json!({"kind": kind});
            add[key] = v.clone();
            refused(&mut s, "layout.addElement", add, &format!("{key} is "));
            let mut update = json!({"element": element});
            update[key] = v.clone();
            refused(&mut s, "layout.update", update, &format!("{key} is "));
        }
    }
    // The title beside it isn't changed either.
    refused(&mut s, "layout.update", json!({"element": bar, "title": "Height", "ticks": "many"}), "ticks is ");
}

#[test]
fn a_frame_shape_of_the_wrong_type_is_refused() {
    let mut s = laid_out();
    for v in wrong() {
        refused(&mut s, "layout.addFrame", json!({"shape": v}), "shape is ");
    }
    refused(&mut s, "layout.addFrame", json!({"shape": "hex"}), "unknown frame shape 'hex'");
}

#[test]
fn a_null_or_valid_colorbar_legend_or_shape_value_is_read_as_before() {
    let mut s = with_keys();
    let bar = id(&s, |k| matches!(k, ElementKind::Colorbar { .. }));
    let legend = id(&s, |k| matches!(k, ElementKind::Legend { .. }));
    let kept = serde_json::to_value(elements(&s)).unwrap();
    s.run("layout.update", &json!({"element": bar, "tickSide": null, "ticks": null, "layer": null, "title": null})).unwrap();
    s.run("layout.update", &json!({"element": legend, "title": null})).unwrap();
    assert_eq!(serde_json::to_value(elements(&s)).unwrap(), kept);
    s.run("layout.addElement", &json!({"kind": "colorbar", "tickSide": null, "ticks": null, "layer": null, "title": null})).unwrap();
    s.run("layout.addElement", &json!({"kind": "legend", "title": null})).unwrap();
    assert!(matches!(&elements(&s).last().unwrap().kind, ElementKind::Legend { title, .. } if title == "Legend"));
    s.run("layout.update", &json!({"element": bar, "tickSide": "above", "ticks": "7", "layer": "", "title": "Height"})).unwrap();
    let e = elements(&s).iter().find(|e| u64::from(e.id) == bar).unwrap();
    assert!(
        matches!(&e.kind, ElementKind::Colorbar { tick_side: TickSide::Start, ticks: 7, layer: None, title, .. } if title == "Height"),
        "{:?}",
        e.kind
    );
    s.run("layout.addFrame", &json!({"shape": null})).unwrap();
    s.run("layout.addFrame", &json!({"shape": "circle"})).unwrap();
    refused(&mut s, "layout.update", json!({"element": bar, "tickSide": "sideways"}), "unknown tickSide 'sideways'");
    // A chart says what its own title takes, as before.
    s.run("layout.addElement", &json!({"kind": "chart", "layer": "Countries"})).unwrap();
    let chart = id(&s, |k| matches!(k, ElementKind::Chart { .. }));
    refused(&mut s, "layout.update", json!({"element": chart, "title": 17}), "a chart's title is a text");
}
