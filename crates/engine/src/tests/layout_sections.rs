//! Section lines on a layout's map frame (#2863): the commands, what they say, and the capability
//! their layouts and commands require.

use crate::{Project, Session, sample};
use rustgis_geom::Coord;
use rustgis_map::layout::{ElementKind, MAX_SECTIONS, SECTION_COLOR, SECTION_WIDTH, SECTION_WIDTHS, Section, SectionLine};
use serde_json::{Value, json};

/// A layout with a frame of the sample's map round (10, 50), and the frame's id.
fn with_frame() -> (Session, u64) {
    let mut s = Session::new(sample::world_project());
    s.run("layout.new", &json!({"page": "a4", "template": "map"})).unwrap();
    let frame = u64::from(s.project.layouts[0].first_frame().unwrap());
    s.run("layout.update", &json!({"element": frame, "center": [10, 50], "scale": 1e7})).unwrap();
    (s, frame)
}

fn sections(s: &Session, frame: u64) -> Vec<Section> {
    match s.project.layouts[0].element(frame as u32).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { sections, .. }) => sections.clone(),
        _ => panic!("a map frame"),
    }
}

fn said(s: &Session, mark: u64) -> Vec<String> {
    s.notices_since(mark).iter().map(|n| n.text.clone()).collect()
}

#[test]
fn section_lines_are_added_changed_removed_and_undone() {
    let (mut s, frame) = with_frame();
    let mark = s.notice_mark();
    let r = s.run("layout.addElement", &json!({"kind": "section"})).unwrap();
    assert_eq!(r, json!({"element": frame, "section": "A"}));
    let middle = Coord::new(10.0, 50.0);
    assert_eq!(sections(&s, frame), vec![Section::new("A".into(), SectionLine::Horizontal(middle))]);
    let r = s
        .run(
            "layout.addElement",
            &json!({"kind": "section", "frame": frame, "name": "a2", "direction": "vertical", "at": [11, 49], "color": "#1F5C84", "width": 1, "dash": [6, 3], "layout": 0, "text": null}),
        )
        .unwrap();
    assert_eq!(r["section"], json!("a2"));
    s.run("layout.addElement", &json!({"kind": "section"})).unwrap();
    assert_eq!(sections(&s, frame).iter().map(|x| x.name.clone()).collect::<Vec<_>>(), ["A", "a2", "B"]);
    assert!(said(&s, mark).is_empty(), "{:?}", said(&s, mark));
    // Changed by name (any case), the keys not given keeping theirs.
    s.run("layout.update", &json!({"element": frame, "section": {"name": "A2", "newName": "Serra", "at": [9, 51]}})).unwrap();
    let serra = &sections(&s, frame)[1];
    assert_eq!((serra.name.as_str(), serra.line), ("Serra", SectionLine::Vertical(Coord::new(9.0, 51.0))));
    assert_eq!((serra.color.hex(), serra.width, serra.dash.is_some()), ("#1F5C84".to_string(), 1.0, true));
    s.run("layout.update", &json!({"element": frame, "section": {"name": "serra", "direction": "horizontal", "dash": null}})).unwrap();
    assert_eq!(sections(&s, frame)[1].line, SectionLine::Horizontal(Coord::new(9.0, 51.0)), "the place kept");
    assert!(sections(&s, frame)[1].dash.is_none());
    // A copy of the frame has its own; removed, and undone in one step.
    let copy = s.run("layout.duplicateElement", &json!({"element": frame})).unwrap()["element"].as_u64().unwrap();
    s.run("layout.update", &json!({"element": frame, "section": {"name": "Serra", "remove": true}})).unwrap();
    assert_eq!((sections(&s, frame).len(), sections(&s, copy).len()), (2, 3));
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(sections(&s, frame).len(), 3);
    // A bad value is refused, changing nothing else either; so is a change to a locked frame.
    let before = s.project.layouts[0].clone();
    for (bad, key) in [
        (json!({"name": "A", "at": [200, 0]}), "at is"),
        (json!({"name": "A", "at": "here"}), "at is"),
        (json!({"name": "A", "direction": "line"}), "direction is"),
        (json!({"name": "A", "color": 3}), "color"),
        (json!({"name": "A", "width": "thin"}), "width"),
        (json!({"name": "A", "dash": "dotted"}), "dash"),
        (json!({"name": "A", "newName": "b"}), "already has a section line 'B'"),
        (json!({"name": "A", "newName": " "}), "newName"),
        (json!({"name": "A", "newName": "n".repeat(21)}), "at most 20"),
        (json!({"name": "A", "rows": 3}), "rows"),
        (json!({"name": "A", "remove": "x"}), "remove"),
        // A line being removed has no place or width to change: refused rather than dropped.
        (json!({"name": "A", "remove": true, "at": [1, 2]}), "a removed section line takes no \"at\""),
        (json!({"name": "A", "width": 3, "remove": true}), "a removed section line takes no \"width\""),
        (json!({"name": "Z"}), "no section line 'Z'"),
        (json!({"at": [0, 0]}), "name"),
        (json!(false), "section is an object"),
    ] {
        let err = s.run("layout.update", &json!({"element": frame, "x": 3, "section": bad})).unwrap_err();
        assert!(err.contains(key), "{bad}: {err}");
    }
    for (bad, key) in [
        (json!({"name": "b"}), "already has"),
        (json!({"name": 5}), "name"),
        (json!({"direction": "line"}), "direction is"),
        (json!({"at": [0]}), "at is"),
        (json!({"background": {}}), "takes no background"),
        // What it doesn't read, a box or a text's, is refused rather than dropped.
        (json!({"x": 10}), "a section takes no \"x\""),
        (json!({"text": "hi"}), "a section takes no \"text\""),
        (json!({"angle": 30}), "a section takes no \"angle\""),
        (json!({"newName": "b"}), "a section takes no \"newName\""),
    ] {
        let mut p = bad.clone();
        p["kind"] = json!("section");
        assert!(s.run("layout.addElement", &p).unwrap_err().contains(key), "{bad}");
    }
    assert_eq!(s.project.layouts[0], before);
    s.run("layout.update", &json!({"element": frame, "locked": true})).unwrap();
    assert!(s.run("layout.addElement", &json!({"kind": "section", "frame": frame})).is_err());
    assert!(s.run("layout.update", &json!({"element": frame, "section": {"name": "A", "remove": true}})).is_err());
    s.run("layout.update", &json!({"element": frame, "locked": false})).unwrap();
    // Only a section has a direction and a place, and only a map frame holds one; a name given for
    // another kind is ignored, and said so, as it was before sections took one.
    for key in ["direction", "at"] {
        let mut p = json!({"kind": "rectangle"});
        p[key] = json!("x");
        assert!(s.run("layout.addElement", &p).unwrap_err().contains("only sections"), "{key}");
    }
    let mark = s.notice_mark();
    s.run("layout.addElement", &json!({"kind": "rectangle", "name": "x"})).unwrap();
    assert!(said(&s, mark)[0].contains("ignores \"name\""), "{:?}", said(&s, mark));
    // A grid and an extent indicator refuse any key they don't read (#2890).
    for kind in ["grid", "extentIndicator"] {
        let e = s.run("layout.addElement", &json!({"kind": kind, "name": "x"})).unwrap_err();
        assert!(e.contains("takes no \"name\""), "{e}");
    }
    let rect = s.run("layout.addElement", &json!({"kind": "rectangle"})).unwrap()["element"].clone();
    let err = s.run("layout.update", &json!({"element": rect, "section": {"name": "A"}})).unwrap_err();
    assert!(err.contains("only map frames"), "{err}");
    // A width out of range is held to it, and said; the most a frame holds, refused past it.
    let mark = s.notice_mark();
    s.run("layout.update", &json!({"element": frame, "section": {"name": "A", "width": 99}})).unwrap();
    assert_eq!(sections(&s, frame)[0].width, SECTION_WIDTHS.1);
    assert!(said(&s, mark)[0].contains("section width 99 held to 20"), "{:?}", said(&s, mark));
    for _ in sections(&s, frame).len()..MAX_SECTIONS {
        s.run("layout.addElement", &json!({"kind": "section"})).unwrap();
    }
    let err = s.run("layout.addElement", &json!({"kind": "section"})).unwrap_err();
    assert!(err.contains("at most 26"), "{err}");
    assert_eq!((sections(&s, frame)[2].color, sections(&s, frame)[2].width), (SECTION_COLOR, SECTION_WIDTH));
}

/// A section line whose place its frame doesn't show isn't drawn, which adding it, changing it and
/// every export say, a series' PDF with the pages; one drawn says nothing.
#[test]
fn a_section_line_not_drawn_is_said() {
    let (mut s, frame) = with_frame();
    let said_of = |s: &mut Session, run: &mut dyn FnMut(&mut Session)| {
        let mark = s.notice_mark();
        run(s);
        said(s, mark).into_iter().filter(|t| t.contains("section line(s)")).collect::<Vec<_>>()
    };
    let drawn: Vec<(&str, Box<dyn Fn(&mut Session)>)> = vec![
        ("png", Box::new(|s| drop(crate::cmd::layout::layout_bytes(s, 0, false, 96.0, false).unwrap()))),
        ("pdf", Box::new(|s| drop(crate::cmd::layout::layout_bytes(s, 0, true, 96.0, true).unwrap()))),
        ("view", Box::new(|s| drop(crate::cmd::layout::layout_png(s, 0, 96.0, None).unwrap()))),
    ];
    let one = |s: &mut Session, p: Value| s.run("layout.addElement", &p).map(drop).unwrap();
    assert!(said_of(&mut s, &mut |s| one(s, json!({"kind": "section"}))).is_empty());
    for (how, export) in &drawn {
        assert!(said_of(&mut s, &mut |s| export(s)).is_empty(), "{how}");
    }
    let note = said_of(&mut s, &mut |s| one(s, json!({"kind": "section", "at": [-150, 0]})));
    assert_eq!(note.len(), 1, "{note:?}");
    assert!(note[0].starts_with("1 section line(s) not drawn: 'B' of ") && note[0].ends_with("its place is outside the frame"), "{note:?}");
    for (how, export) in &drawn {
        assert_eq!(said_of(&mut s, &mut |s| export(s)), note, "{how}");
    }
    let change = |s: &mut Session, p: Value| s.run("layout.update", &json!({"element": frame, "section": p})).map(drop).unwrap();
    assert!(said_of(&mut s, &mut |s| change(s, json!({"name": "B", "at": [10, 50]}))).is_empty(), "brought into the frame");
    assert_eq!(said_of(&mut s, &mut |s| change(s, json!({"name": "B", "at": [-150, 0]}))), note);
    assert!(said_of(&mut s, &mut |s| change(s, json!({"name": "B", "remove": true}))).is_empty());
    // On a hidden frame nothing is drawn, so nothing is missing.
    one(&mut s, json!({"kind": "section", "at": [-150, 0]}));
    s.run("layout.update", &json!({"element": frame, "visible": false})).unwrap();
    assert!(said_of(&mut s, &mut |s| drop(crate::cmd::layout::layout_bytes(s, 0, false, 96.0, false).unwrap())).is_empty());
    s.run("layout.update", &json!({"element": frame, "visible": true})).unwrap();
    // A series moves the frame, and the line stays at its place: the PDF says how many pages lack
    // it, and the layout is left as it was.
    change(&mut s, json!({"name": "B", "remove": true}));
    s.run("layout.series", &json!({"layer": "Lakes"})).unwrap();
    let before = s.project.layouts[0].clone();
    let note = said_of(&mut s, &mut |s| drop(crate::cmd::layout::layout_bytes(s, 0, true, 36.0, true).unwrap()));
    assert!(note.len() == 1 && note[0].contains("'A' of ") && note[0].contains(" pages: its place"), "{note:?}");
    assert_eq!(s.project.layouts[0], before);
    // On the globe some pages are too far round for its place, others only beside it: one line all
    // the same, each reason with its pages.
    s.project.maps[0].projection = rustgis_map::Projection::Globe;
    let note = said_of(&mut s, &mut |s| drop(crate::cmd::layout::layout_bytes(s, 0, true, 36.0, true).unwrap()));
    assert_eq!(note.len(), 1, "{note:?}");
    assert!(note[0].starts_with("1 section line(s) not drawn: 'A' of ") && note[0].matches("'A' of ").count() == 1, "{note:?}");
    for why in [" pages: its place is outside the frame", " pages: its place isn't one the frame's map shows"] {
        assert!(note[0].contains(why), "{note:?}");
    }
}

/// A report's layout page is drawn as the layout's own export is, and says the same of a section
/// line it can't draw, once a layout, as an image as in a PDF.
#[test]
fn a_report_says_a_section_line_its_layout_page_does_not_draw() {
    let (mut s, _) = with_frame();
    s.run("layout.addElement", &json!({"kind": "section", "at": [-150, 0]})).unwrap();
    let layout = s.project.layouts[0].name.clone();
    s.run("report.new", &json!({"layer": "Countries", "name": "R", "fields": ["name"], "where": "name LIKE 'F%'"})).unwrap();
    s.run("report.existingTemplate", &json!({"report": "R", "layout": layout})).unwrap();
    s.run("report.existingTemplate", &json!({"report": "R", "layout": layout, "end": true})).unwrap();
    /// Removed however the test ends.
    struct Dir(std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
    let dir = Dir(std::env::temp_dir().join(format!("rustgis-report-sections-{}", std::process::id())));
    std::fs::create_dir_all(&dir.0).unwrap();
    for file in ["r.pdf", "r.png"] {
        let mark = s.notice_mark();
        s.run("report.export", &json!({"report": "R", "path": dir.0.join(file).display().to_string(), "dpi": 36})).unwrap();
        let note: Vec<_> = said(&s, mark).into_iter().filter(|t| t.contains("section line(s)")).collect();
        assert_eq!(note.len(), 1, "{file}: {note:?}");
        assert!(
            note[0].contains(&format!("'{layout}'")) && note[0].contains("'A' of ") && note[0].ends_with("its place is outside the frame"),
            "{note:?}"
        );
    }
    // Two layouts that each leave one out: one notice counting both, each named with its layout.
    s.run("layout.new", &json!({"page": "a4", "template": "map"})).unwrap();
    let second = s.project.layouts[1].name.clone();
    let other = u64::from(s.project.layouts[1].first_frame().unwrap());
    s.run("layout.update", &json!({"layout": 1, "element": other, "center": [10, 50], "scale": 1e7})).unwrap();
    s.run("layout.addElement", &json!({"layout": 1, "kind": "section", "at": [-150, 0]})).unwrap();
    s.run("report.existingTemplate", &json!({"report": "R", "layout": second})).unwrap();
    let mark = s.notice_mark();
    s.run("report.export", &json!({"report": "R", "path": dir.0.join("r.pdf").display().to_string(), "dpi": 36})).unwrap();
    let note: Vec<_> = said(&s, mark).into_iter().filter(|t| t.contains("section line(s)")).collect();
    assert_eq!(note.len(), 1, "{note:?}");
    assert!(
        note[0].contains("2 section line(s) not drawn")
            && note[0].contains(&format!("'{layout}'"))
            && note[0].contains(&format!("'{second}'")),
        "{note:?}"
    );
    s.run("layout.update", &json!({"layout": 1, "element": other, "section": {"name": "A", "remove": true}})).unwrap();
    // Brought into the frame, nothing is said.
    let frame = u64::from(s.project.layouts[0].first_frame().unwrap());
    s.run("layout.update", &json!({"layout": 0, "element": frame, "section": {"name": "A", "at": [10, 50]}})).unwrap();
    let mark = s.notice_mark();
    s.run("report.export", &json!({"report": "R", "path": dir.0.join("r.pdf").display().to_string(), "dpi": 36})).unwrap();
    assert!(!said(&s, mark).iter().any(|t| t.contains("section line(s)")));
}

/// Section lines are required where they are saved, so an older reader refuses the project instead
/// of drawing its frames without them and saving it so. A layout without one needs nothing.
#[test]
fn section_capability_follows_saved_layouts_and_commands() {
    let fixture = Project::from_json(include_str!("../../../../schema/fixtures/layout_section.rgis")).unwrap();
    let saved = serde_json::to_value(&fixture).unwrap();
    assert_eq!(saved["requires"], json!(["layout-section"]));
    let frames = &saved["layouts"][0]["elements"];
    assert!(frames[0]["kind"]["MapFrame"].get("sections").is_none());
    let file: Value = serde_json::from_str(include_str!("../../../../schema/fixtures/layout_section.rgis")).unwrap();
    assert_eq!(
        frames[1]["kind"]["MapFrame"]["sections"][1]["name"],
        file["layouts"][0]["elements"][1]["kind"]["MapFrame"]["sections"][1]["name"]
    );
    assert_eq!(Project::from_json(&saved.to_string()).unwrap().layouts, fixture.layouts);
    let mut clean = fixture;
    for e in &mut clean.layouts[0].elements {
        if let ElementKind::MapFrame { sections, .. } = &mut e.kind {
            sections.clear();
        }
    }
    let base = serde_json::to_value(&clean).unwrap();
    assert!(base.get("requires").is_none(), "no section lines need nothing");
    let layout = |v: &Value| json!({"text": v["layouts"][0].to_string()});
    for (command, params, required) in [
        ("layout.addElement", json!({"kind": "section", "name": "A"}), true),
        ("layout.addElement", json!({"kind": "grid"}), false),
        ("layout.update", json!({"element": 2, "section": {"name": "A", "width": 2}}), true),
        ("layout.update", json!({"element": 2, "section": null}), false),
        ("layout.import", layout(&saved), true),
        ("layout.import", layout(&base), false),
        // A report's template is a layout file read as `layout.import` reads it.
        ("report.existingTemplate", layout(&saved), true),
        ("report.existingTemplate", layout(&base), false),
    ] {
        let script = format!("{command} {params}");
        for step in [
            json!({"title": "S", "command": command, "params": params}),
            json!({"title": "S", "command": "script.run", "params": {"script": script}}),
        ] {
            let mut document = base.clone();
            document["tasks"] = json!([{"name": "Task", "steps": [step]}]);
            let saved = serde_json::to_value(Project::from_json(&document.to_string()).unwrap()).unwrap();
            let expected = if required { json!(["layout-section"]) } else { Value::Null };
            assert_eq!(saved.get("requires").cloned().unwrap_or_default(), expected, "{command} {params}");
        }
    }
    let mut unknown = saved;
    unknown["requires"] = json!(["layout-section", "future-section"]);
    assert!(Project::from_json(&unknown.to_string()).unwrap_err().contains("future-section"));
}
