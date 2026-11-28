//! A legend that shows how many features each entry has (#3237).

use super::*;

/// A world layout and its legend.
fn laid_out() -> (Session, u64) {
    let mut s = world();
    s.run("layout.new", &json!({"template": "map"})).unwrap();
    let legend = s.project.layouts[0].elements.iter().find(|e| e.kind.code() == "legend").unwrap().id.into();
    (s, legend)
}

/// The legend's saved setting.
fn saved(s: &Session, legend: u64) -> Value {
    let e = s.project.layouts[0].element(legend as u32).unwrap();
    serde_json::to_value(&e.kind).unwrap()["Legend"]["show_counts"].clone()
}

#[test]
fn counts_are_turned_on_in_one_undo_step_and_left_out_when_off() {
    let (mut s, legend) = laid_out();
    assert_eq!(saved(&s, legend), Value::Null);
    let depth = s.undo_depth();
    s.run("layout.update", &json!({"element": legend, "showCounts": true})).unwrap();
    assert_eq!(saved(&s, legend), json!(true));
    assert_eq!(s.undo_depth(), depth + 1);
    // `null` leaves it as it is.
    s.run("layout.update", &json!({"element": legend, "showCounts": null})).unwrap();
    assert_eq!(saved(&s, legend), json!(true));
    s.run("layout.update", &json!({"element": legend, "showCounts": false})).unwrap();
    assert_eq!(saved(&s, legend), Value::Null);
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(saved(&s, legend), json!(true));
}

#[test]
fn a_legend_is_added_with_counts() {
    let (mut s, _) = laid_out();
    s.run("layout.addElement", &json!({"kind": "legend", "showCounts": true})).unwrap();
    let added = s.project.layouts[0].elements.last().unwrap().id.into();
    assert_eq!(saved(&s, added), json!(true));
}

#[test]
fn counts_a_legend_cannot_take_are_refused_and_change_nothing() {
    let (mut s, legend) = laid_out();
    let frame = s.project.layouts[0].elements.iter().find(|e| e.kind.code() == "mapFrame").unwrap().id;
    for (command, p, said) in [
        ("layout.update", json!({"element": legend, "showCounts": "yes"}), "showCounts is true or false"),
        ("layout.addElement", json!({"kind": "legend", "showCounts": 1}), "showCounts is true or false"),
        ("layout.update", json!({"element": frame, "showCounts": true}), "only legends take showCounts"),
        ("layout.addElement", json!({"kind": "northArrow", "showCounts": true}), "only legends take showCounts"),
    ] {
        let before = serde_json::to_value(&s.project).unwrap();
        let err = s.run(command, &p).expect_err(&p.to_string()).to_string();
        assert!(err.contains(said), "{p}: {err}");
        assert_eq!(serde_json::to_value(&s.project).unwrap(), before, "{p}");
    }
}

#[test]
fn counts_require_their_capability_wherever_they_are_saved() {
    let (mut s, legend) = laid_out();
    let lists = |v: Value| v["requires"].as_array().is_some_and(|r| r.contains(&json!("layout-legend-counts")));
    assert!(!lists(serde_json::to_value(&s.project).unwrap()));
    let plain = serde_json::to_string(&s.project.layouts[0]).unwrap();
    s.run("layout.update", &json!({"element": legend, "showCounts": true})).unwrap();
    assert!(lists(serde_json::to_value(&s.project).unwrap()));
    let text = serde_json::to_string(&s.project).unwrap();
    let again: Project = serde_json::from_str(&text).unwrap();
    assert_eq!(serde_json::to_string(&again).unwrap(), text);
    let counted = serde_json::to_string(&s.project.layouts[0]).unwrap();
    let requires = |id: &str, params: Value| {
        let mut p = Project::default();
        p.tasks.push(
            serde_json::from_value(json!({"name": "t", "title": "t", "steps": [{"title": "s", "command": id, "params": params}]})).unwrap(),
        );
        lists(serde_json::to_value(&p).unwrap())
    };
    for (id, params, required) in [
        ("layout.addElement", json!({"kind": "legend", "showCounts": true}), true),
        ("layout.update", json!({"element": 3, "showCounts": true}), true),
        ("layout.update", json!({"element": 3, "showCounts": false}), false),
        ("layout.update", json!({"element": 3, "showCounts": null}), false),
        ("layout.import", json!({"text": counted}), true),
        ("layout.import", json!({"text": plain}), false),
        ("report.existingTemplate", json!({"text": counted}), true),
        ("script.run", json!({"script": format!("layout.update {}", json!({"element": 3, "showCounts": true}))}), true),
    ] {
        assert_eq!(requires(id, params.clone()), required, "{id} {params}");
    }
}

#[test]
fn a_files_value_that_is_not_a_boolean_reads_as_off_and_is_counted() {
    let (s, legend) = laid_out();
    let mut layout = serde_json::to_value(&s.project.layouts[0]).unwrap();
    let elements = layout["elements"].as_array_mut().unwrap();
    let e = elements.iter_mut().find(|e| e["id"] == json!(legend)).unwrap();
    e["kind"]["Legend"]["show_counts"] = json!("yes");
    let mut read: rustgis_map::layout::Layout = serde_json::from_value(layout).unwrap();
    assert_eq!(read.clamp().elements_adjusted, 1);
    let e = read.element(legend as u32).unwrap();
    assert_eq!(serde_json::to_value(&e.kind).unwrap()["Legend"]["show_counts"], Value::Null);
}

#[test]
fn counts_that_overflow_a_fitted_legend_say_so() {
    let (mut s, legend) = laid_out();
    s.run("layout.update", &json!({"element": legend, "w": 5, "h": 90, "fit": "adjustSize"})).unwrap();
    s.notices.clear();
    s.run("layout.update", &json!({"element": legend, "showCounts": true})).unwrap();
    assert!(s.notices.iter().any(|n| n.text.contains("even with its text shrunk to 4 points")), "{:?}", s.notices);
}
