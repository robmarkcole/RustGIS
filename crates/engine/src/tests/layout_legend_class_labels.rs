//! A legend's own labels for a layer's classes (#3187).

use super::*;

/// A layout whose legend lists Countries by continent and Rivers in one symbol.
fn laid_out() -> (Session, u64) {
    let mut s = world();
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "unique", "field": "continent"})).unwrap();
    s.run("layout.new", &json!({"template": "map"})).unwrap();
    let legend = s.project.layouts[0].elements.iter().find(|e| e.kind.code() == "legend").unwrap().id.into();
    (s, legend)
}

/// The legend's saved settings.
fn saved(s: &Session, legend: u64) -> Value {
    let e = s.project.layouts[0].element(legend as u32).unwrap();
    serde_json::to_value(&e.kind).unwrap()["Legend"].clone()
}

fn labels(s: &Session, legend: u64) -> Value {
    saved(s, legend)["class_labels"].clone()
}

fn give(s: &mut Session, legend: u64, given: Value) {
    s.run("layout.update", &json!({"element": legend, "classLabels": given})).unwrap();
}

#[test]
fn a_class_is_renamed_in_one_legend_alone() {
    let (mut s, legend) = laid_out();
    let layer = serde_json::to_value(&s.map().layers).unwrap();
    let depth = s.undo_depth();
    give(&mut s, legend, json!({" countries ": {"Africa": "Afrika", "<all other values>": "Elsewhere"}}));
    // Kept under the layer's name as the map spells it.
    assert_eq!(labels(&s, legend), json!({"Countries": {"<all other values>": "Elsewhere", "Africa": "Afrika"}}));
    assert_eq!(serde_json::to_value(&s.map().layers).unwrap(), layer);
    assert_eq!(s.undo_depth(), depth + 1);
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(labels(&s, legend), Value::Null);
}

#[test]
fn a_legend_is_added_with_class_labels() {
    let (mut s, _) = laid_out();
    let given = json!({"Countries": {"Asia": "Asien", "Africa": ""}});
    s.run("layout.addElement", &json!({"kind": "legend", "classLabels": given})).unwrap();
    let added = s.project.layouts[0].elements.last().unwrap().id.into();
    assert_eq!(labels(&s, added), json!({"Countries": {"Asia": "Asien"}}));
}

#[test]
fn a_graduated_range_is_renamed_by_the_label_it_shows() {
    let (mut s, legend) = laid_out();
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "graduated", "field": "pop_est", "classes": 3})).unwrap();
    let first = s.map().layer_by_name("Countries").unwrap().renderer.legend()[0].label.clone();
    give(&mut s, legend, json!({"Countries": {first.clone(): "Small"}}));
    assert_eq!(labels(&s, legend), json!({"Countries": {first: "Small"}}));
}

#[test]
fn a_blank_or_null_label_takes_a_rename_off() {
    let (mut s, legend) = laid_out();
    give(&mut s, legend, json!({"Countries": {"Africa": "A", "Asia": "B", "Europe": "C"}}));
    // Each entry replaces that class's label; the others stay.
    give(&mut s, legend, json!({"Countries": {"Africa": null, "Asia": "  ", "Europe": "D"}}));
    assert_eq!(labels(&s, legend), json!({"Countries": {"Europe": "D"}}));
    // A class the layer no longer lists can still be taken off.
    s.run("layer.symbology", &json!({"layer": "Countries", "type": "unique", "field": "name"})).unwrap();
    give(&mut s, legend, json!({"Countries": {"Europe": null}}));
    assert_eq!(labels(&s, legend), Value::Null);
    // A layer's, or every layer's, at once.
    for all in [json!({"Countries": null}), json!({"Countries": {}}), json!({})] {
        give(&mut s, legend, json!({"Countries": {"France": "F"}}));
        give(&mut s, legend, all.clone());
        assert_eq!(labels(&s, legend), Value::Null, "{all}");
    }
    // `null` leaves them as they are.
    give(&mut s, legend, json!({"Countries": {"France": "F"}}));
    give(&mut s, legend, Value::Null);
    assert_eq!(labels(&s, legend), json!({"Countries": {"France": "F"}}));
}

#[test]
fn a_class_label_the_legend_cannot_use_is_refused_and_changes_nothing() {
    let (mut s, legend) = laid_out();
    let long = "x".repeat(201);
    for (given, said) in [
        (json!({"Roads": {"a": "b"}}), "no layer 'Roads'"),
        (json!({"Countries": {"africa": "b"}}), "'Countries' lists no class 'africa'"),
        (json!({"Rivers": {"a": "b"}}), "'Rivers' has no classes to rename; give its name in labels"),
        (json!({"Countries": {"Africa": long}}), "at most 200 characters"),
        (json!({"Countries": {"Africa": 3}}), "classLabels is "),
        (json!({"Countries": "Africa"}), "classLabels is "),
        (json!(["Countries"]), "classLabels is "),
        (json!({"Countries": {"Africa": "a"}, "countries": {"Asia": "b"}}), "names layer 'Countries' twice"),
    ] {
        let before = serde_json::to_value(&s.project).unwrap();
        let undo = s.undo_label();
        for (command, p) in [
            ("layout.update", json!({"element": legend, "title": "Changed", "classLabels": given})),
            ("layout.addElement", json!({"kind": "legend", "classLabels": given})),
        ] {
            let err = s.run(command, &p).expect_err(&p.to_string()).to_string();
            assert!(err.contains(said), "{p}: {err}");
            assert_eq!(serde_json::to_value(&s.project).unwrap(), before, "{p}");
            assert_eq!(s.undo_label(), undo, "{p}");
        }
    }
}

#[test]
fn only_legends_take_class_labels() {
    let (mut s, _) = laid_out();
    let frame = s.project.layouts[0].elements.iter().find(|e| e.kind.code() == "mapFrame").unwrap().id;
    let given = json!({"Countries": {"Africa": "a"}});
    for (command, p) in [
        ("layout.update", json!({"element": frame, "classLabels": given})),
        ("layout.addElement", json!({"kind": "text", "text": "t", "classLabels": given})),
    ] {
        let err = s.run(command, &p).expect_err(&p.to_string()).to_string();
        assert!(err.contains("only legends take classLabels"), "{p}: {err}");
    }
}

#[test]
fn class_labels_follow_a_renamed_or_removed_layer() {
    let (mut s, legend) = laid_out();
    give(&mut s, legend, json!({"Countries": {"Africa": "a"}}));
    s.run("layer.rename", &json!({"layer": "Countries", "name": "Nations"})).unwrap();
    assert_eq!(labels(&s, legend), json!({"Nations": {"Africa": "a"}}));
    s.run("layer.remove", &json!({"layer": "Nations"})).unwrap();
    assert_eq!(labels(&s, legend), Value::Null);
}

#[test]
fn a_fit_that_class_labels_overflow_says_so() {
    let (mut s, legend) = laid_out();
    s.run("layout.update", &json!({"element": legend, "w": 60, "h": 90, "fit": "adjustSize"})).unwrap();
    s.notices.clear();
    give(&mut s, legend, json!({"Countries": {"Africa": "W".repeat(200)}}));
    assert!(s.notices.iter().any(|n| n.text.contains("even with its text shrunk to 4 points")), "{:?}", s.notices);
}

#[test]
fn class_labels_require_their_capability_and_reopen() {
    let (mut s, legend) = laid_out();
    let requires = |s: &Session| serde_json::to_value(&s.project).unwrap()["requires"].clone();
    let lists = |v: &Value| v.as_array().is_some_and(|r| r.contains(&json!("layout-legend-class-labels")));
    assert!(!lists(&requires(&s)), "{}", requires(&s));
    give(&mut s, legend, json!({"Countries": {"Africa": "a"}}));
    assert!(lists(&requires(&s)), "{}", requires(&s));
    let text = serde_json::to_string(&s.project).unwrap();
    let again: Project = serde_json::from_str(&text).unwrap();
    assert_eq!(serde_json::to_string(&again).unwrap(), text);
}

#[test]
fn saved_commands_and_layout_files_with_class_labels_require_the_capability() {
    let (mut s, legend) = laid_out();
    give(&mut s, legend, json!({"Countries": {"Africa": "a"}}));
    let layout = serde_json::to_string(&s.project.layouts[0]).unwrap();
    let requires = |id: &str, params: Value| {
        let mut p = Project::default();
        p.tasks.push(
            serde_json::from_value(json!({"name": "t", "title": "t", "steps": [{"title": "s", "command": id, "params": params}]})).unwrap(),
        );
        serde_json::to_value(&p).unwrap()["requires"].as_array().is_some_and(|r| r.contains(&json!("layout-legend-class-labels")))
    };
    let given = json!({"Countries": {"Africa": "a"}});
    // A layout file whose labels come to nothing needs nothing, as the commands that give none.
    let emptied = |classes: Value| {
        let mut l: Value = serde_json::from_str(&layout).unwrap();
        let elements = l["elements"].as_array_mut().unwrap();
        let e = elements.iter_mut().find(|e| e["kind"]["Legend"].is_object()).unwrap();
        e["kind"]["Legend"]["class_labels"] = classes;
        json!({"text": l.to_string()})
    };
    for classes in [json!({"Countries": {}}), json!({"Countries": {"Africa": " "}})] {
        assert!(!requires("layout.import", emptied(classes.clone())), "{classes}");
    }
    for (id, params, required) in [
        ("layout.addElement", json!({"kind": "legend", "classLabels": given}), true),
        ("layout.update", json!({"element": 3, "classLabels": given}), true),
        ("layout.update", json!({"element": 3, "classLabels": {"Countries": {"Africa": null, "Asia": " "}}}), false),
        ("layout.update", json!({"element": 3, "classLabels": {"Countries": null}}), false),
        ("layout.update", json!({"element": 3, "classLabels": {}}), false),
        ("layout.update", json!({"element": 3, "classLabels": null}), false),
        ("layout.import", json!({"text": layout}), true),
        ("script.run", json!({"script": format!("layout.update {}", json!({"element": 3, "classLabels": given}))}), true),
    ] {
        assert_eq!(requires(id, params.clone()), required, "{id} {params}");
    }
}

#[test]
fn class_labels_lost_to_a_shared_name_are_said() {
    let (mut s, legend) = laid_out();
    give(&mut s, legend, json!({"Countries": {"Africa": "a"}}));
    s.notices.clear();
    s.run("layer.rename", &json!({"layer": "Countries", "name": "Rivers"})).unwrap();
    assert_eq!(labels(&s, legend), Value::Null);
    let said = "'Countries', renamed 'Rivers', lost its class labels in 1 legend(s)";
    assert!(s.notices.iter().any(|n| n.text.contains(said)), "{:?}", s.notices);
}
