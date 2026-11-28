//! A presentation map page whose map the project doesn't have is drawn over the active map:
//! deleting the map, importing such a presentation (#3047) or opening a project that has such pages
//! (#3191) says which pages. All keep the name.

use super::*;
use rustgis_map::presentation::{Page, PageKind, Presentation};

/// The notices `command` gives about map pages that show a map the project doesn't have.
fn missing(s: &mut Session, command: &str, params: Value) -> Vec<String> {
    let mark = s.notice_mark();
    s.run(command, &params).unwrap();
    s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains("which the project doesn't have")).collect()
}

/// The sample world with a map `Other`, the world in view, and a presentation `Tour`.
fn with_tour() -> Session {
    let mut s = world();
    let home = s.map().name.clone();
    s.run("map.new", &json!({"name": "Other"})).unwrap();
    s.run("map.activate", &json!({"name": home})).unwrap();
    s.run("presentation.new", &json!({"name": "Tour"})).unwrap();
    s
}

fn page(s: &mut Session, map: &str) {
    s.run("presentation.mapPage", &json!({"map": map, "title": "Sites"})).unwrap();
}

/// The map the frame of page `k` of the first presentation is drawn over.
fn drawn_over(s: &Session, k: usize) -> String {
    let pres = &s.project.presentations[0];
    let layout = crate::cmd::presentation::page_layout(s, pres, &pres.pages[k]);
    layout
        .elements
        .iter()
        .find_map(|e| match &e.kind {
            rustgis_map::layout::ElementKind::MapFrame { map, .. } => Some(map.clone()),
            _ => None,
        })
        .unwrap()
}

/// A presentation file with one map page for each of `maps`.
fn file_showing(maps: &[&str]) -> Value {
    let pages = maps
        .iter()
        .map(|map| Page {
            title: "Sites".into(),
            kind: PageKind::Map { map: (*map).into(), center: rustgis_geom::Coord::new(0.0, 0.0), width_m: 1000.0 },
            notes: String::new(),
            seconds: 5.0,
        })
        .collect();
    let pres = Presentation { name: "Filed".into(), pages, ..Presentation::default() };
    json!({"text": serde_json::to_string(&pres).unwrap()})
}

#[test]
fn deleting_a_map_a_page_shows_says_so_and_the_page_keeps_the_name() {
    let mut s = with_tour();
    page(&mut s, "Other");
    assert_eq!(
        missing(&mut s, "map.remove", json!({"name": "Other"})),
        [
            "1 map page shows 'Other', which the project doesn't have, so it is drawn over the active map until a map of that name comes back: presentation 'Tour', page 2"
        ]
    );
    assert_eq!(drawn_over(&s, 1), s.map().name);
    // Exporting draws it as before, with nothing more said.
    let mark = s.notice_mark();
    crate::cmd::presentation::export_bytes(&mut s, 0, "html", 120.0).unwrap();
    assert!(s.notices_since(mark).iter().all(|n| !n.text.contains("which the project doesn't have")));
    // A map of that name brings the page back to it.
    s.run("map.new", &json!({"name": "Other"})).unwrap();
    assert_eq!(drawn_over(&s, 1), "Other");
}

#[test]
fn undoing_the_deletion_draws_the_page_over_its_map_again() {
    let mut s = with_tour();
    page(&mut s, "Other");
    s.run("map.remove", &json!({"name": "Other"})).unwrap();
    assert_eq!(missing(&mut s, "edit.undo", Value::Null), Vec::<String>::new());
    assert_eq!(drawn_over(&s, 1), "Other");
}

#[test]
fn pages_of_two_presentations_are_said_once() {
    let mut s = with_tour();
    page(&mut s, "Other");
    s.run("presentation.new", &json!({"name": "Second"})).unwrap();
    page(&mut s, "Other");
    assert_eq!(
        missing(&mut s, "map.remove", json!({"name": "Other"})),
        [
            "2 map pages show 'Other', which the project doesn't have, so they are drawn over the active map until a map of that name comes back: presentation 'Tour', page 2; presentation 'Second', page 2"
        ]
    );
}

#[test]
fn a_page_that_spells_its_map_in_another_case_is_counted() {
    let mut s = with_tour();
    page(&mut s, "Other");
    let PageKind::Map { map, .. } = &mut s.project.presentations[0].pages[1].kind else { panic!("a map page") };
    *map = "oTHER".into();
    assert_eq!(
        missing(&mut s, "map.remove", json!({"name": "Other"})),
        [
            "1 map page shows 'oTHER', which the project doesn't have, so it is drawn over the active map until a map of that name comes back: presentation 'Tour', page 2"
        ]
    );
}

#[test]
fn deleting_a_map_no_page_shows_says_nothing() {
    let mut s = with_tour();
    s.run("map.new", &json!({"name": "Spare"})).unwrap();
    page(&mut s, "Other");
    // A page already without its map isn't said again when another map goes.
    let PageKind::Map { map, .. } = &mut s.project.presentations[0].pages[1].kind else { panic!("a map page") };
    *map = "Gone".into();
    assert_eq!(missing(&mut s, "map.remove", json!({"name": "Spare"})), Vec::<String>::new());
}

#[test]
fn deleting_a_map_with_no_presentation_says_nothing() {
    let mut s = world();
    s.run("map.new", &json!({"name": "Other"})).unwrap();
    assert_eq!(missing(&mut s, "map.remove", json!({"name": "Other"})), Vec::<String>::new());
}

/// Adds the offline area `Crew`, made from the map `source`.
fn offline_area(s: &mut Session, source: &str) {
    let extent = rustgis_geom::Extent::new(0.0, 0.0, 1.0, 1.0);
    let area = json!({"name": "Crew", "map": source, "extent": extent, "created": 0.0, "data": s.project.maps[0]});
    s.project.offline_areas.push(serde_json::from_value(area).unwrap());
}

#[test]
fn removing_an_offline_area_says_the_pages_that_showed_its_map() {
    let mut s = with_tour();
    let home = s.map().name.clone();
    s.run("map.new", &json!({"name": "Crew (offline)"})).unwrap();
    offline_area(&mut s, &home);
    page(&mut s, "Crew (offline)");
    assert_eq!(
        missing(&mut s, "offline.remove", json!({"name": "Crew"})),
        [
            "1 map page shows 'Crew (offline)', which the project doesn't have, so it is drawn over the active map until a map of that name comes back: presentation 'Tour', page 2"
        ]
    );
}

#[test]
fn removing_an_offline_area_whose_map_is_kept_says_nothing() {
    let mut s = world();
    s.run("map.rename", &json!({"name": "Crew (offline)"})).unwrap();
    s.run("presentation.new", &json!({"name": "Tour"})).unwrap();
    page(&mut s, "Crew (offline)");
    offline_area(&mut s, "Source");
    assert_eq!(missing(&mut s, "offline.remove", json!({"name": "Crew"})), Vec::<String>::new());
    assert_eq!(drawn_over(&s, 1), "Crew (kept)");
}

#[test]
fn importing_a_presentation_says_the_pages_whose_map_is_missing() {
    let mut s = world();
    let home = s.map().name.clone();
    assert_eq!(
        missing(&mut s, "presentation.import", file_showing(&["Missing", &home, "missing"])),
        [
            "2 map pages show 'Missing', which the project doesn't have, so they are drawn over the active map until a map of that name comes back: presentation 'Filed', pages 1, 3"
        ]
    );
}

#[test]
fn importing_a_presentation_whose_maps_are_all_there_says_nothing() {
    let mut s = world();
    let home = s.map().name.clone();
    assert_eq!(missing(&mut s, "presentation.import", file_showing(&[&home])), Vec::<String>::new());
}

#[test]
fn many_missing_maps_are_named_up_to_three() {
    let mut s = world();
    assert_eq!(
        missing(&mut s, "presentation.import", file_showing(&["A", "B", "A", "C", "D", "E"])),
        [
            "6 map pages show 'A', 'B', 'C', …, which the project doesn't have, so they are drawn over the active map until maps of those names come back: presentation 'Filed', pages 1, 2, 3, 4, 5, …"
        ]
    );
}

#[test]
fn pages_past_three_presentations_are_counted_and_not_listed() {
    let mut s = with_tour();
    // Five pages in the first presentation, all listed; one in each of three more.
    for _ in 0..5 {
        page(&mut s, "Other");
    }
    for name in ["Second", "Third", "Fourth"] {
        s.run("presentation.new", &json!({"name": name})).unwrap();
        page(&mut s, "Other");
    }
    assert_eq!(
        missing(&mut s, "map.remove", json!({"name": "Other"})),
        [
            "8 map pages show 'Other', which the project doesn't have, so they are drawn over the active map until a map of that name comes back: presentation 'Tour', pages 2, 3, 4, 5, 6; presentation 'Second', page 2; presentation 'Third', page 2; …"
        ]
    );
}

const GONE_ON_PAGE_2: &str = "1 map page shows 'Gone', which the project doesn't have, so it is drawn over the active map until a map of that name comes back: presentation 'Tour', page 2";

/// The project of [`with_tour`] with a map page showing `map`, which no command checks any more.
fn project_showing(map: &str) -> Session {
    let mut s = with_tour();
    page(&mut s, "Other");
    let PageKind::Map { map: shown, .. } = &mut s.project.presentations[0].pages[1].kind else { panic!("a map page") };
    *shown = map.into();
    s
}

fn text_of(s: &Session) -> Value {
    json!({"text": serde_json::to_string(&s.project).unwrap()})
}

/// The notices about missing page maps among those `s` holds.
fn held(s: &Session) -> Vec<String> {
    s.notices.iter().map(|n| n.text.clone()).filter(|t| t.contains("which the project doesn't have")).collect()
}

#[test]
fn opening_a_project_says_the_pages_whose_map_is_missing() {
    let text = text_of(&project_showing("Gone"));
    let mut s = Session::new(Project::default());
    assert_eq!(missing(&mut s, "project.openText", text), [GONE_ON_PAGE_2]);
    let PageKind::Map { map, .. } = &s.project.presentations[0].pages[1].kind else { panic!("a map page") };
    assert_eq!(map, "Gone");
}

#[test]
fn opening_a_saved_file_after_deleting_the_map_says_so() {
    let mut saved = with_tour();
    page(&mut saved, "Other");
    saved.run("map.remove", &json!({"name": "Other"})).unwrap();
    let dir = std::env::temp_dir().join(format!("rustgis-pages-without-map-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tour.rgis");
    saved.project.save(&path).unwrap();
    let mut s = Session::new(Project::default());
    let said = missing(&mut s, "project.open", json!({"path": path}));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(said, [GONE_ON_PAGE_2.replace("'Gone'", "'Other'")]);
}

#[test]
fn a_session_started_with_such_a_project_says_so() {
    let text = serde_json::to_string(&project_showing("Gone").project).unwrap();
    let s = Session::new(Project::from_json(&text).unwrap());
    assert_eq!(held(&s), [GONE_ON_PAGE_2]);
}

#[test]
fn a_peer_opening_such_a_project_says_so() {
    let text = serde_json::to_string(&project_showing("Gone").project).unwrap();
    let mut s = Session::new(Project::default());
    s.open_peer_text(crate::Peer::Embedder, &text).unwrap();
    assert_eq!(held(&s), [GONE_ON_PAGE_2]);
}

#[test]
fn opening_a_project_whose_pages_have_their_maps_says_nothing() {
    let text = text_of(&project_showing("oTHER"));
    let mut s = Session::new(Project::default());
    assert_eq!(missing(&mut s, "project.openText", text), Vec::<String>::new());
}

#[test]
fn opening_a_project_with_no_presentation_says_nothing() {
    let text = text_of(&world());
    let mut s = Session::new(Project::default());
    assert_eq!(missing(&mut s, "project.openText", text), Vec::<String>::new());
}

#[test]
fn opening_a_project_says_the_pages_of_two_presentations_once() {
    let mut saved = project_showing("Gone");
    saved.run("presentation.new", &json!({"name": "Second"})).unwrap();
    let gone = saved.project.presentations[0].pages[1].clone();
    saved.project.presentations[1].pages.push(gone);
    let mut s = Session::new(Project::default());
    assert_eq!(
        missing(&mut s, "project.openText", text_of(&saved)),
        [
            "2 map pages show 'Gone', which the project doesn't have, so they are drawn over the active map until a map of that name comes back: presentation 'Tour', page 2; presentation 'Second', page 2"
        ]
    );
}

#[test]
fn importing_the_maps_of_such_a_project_says_nothing_of_its_pages() {
    let text = text_of(&project_showing("Gone"));
    let mut s = world();
    assert_eq!(missing(&mut s, "map.import", text), Vec::<String>::new());
}
