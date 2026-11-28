//! Headline figures on `layout.publication`'s panel style (#3263): stacked in the side panel under
//! the title and subtitle, over the locator and the legend.

use super::*;
use rustgis_map::layout::{Element, ElementKind, Layout};

const PIAUI: [f64; 4] = [-46.8, -11.6, -38.5, -2.2];

/// A panel sheet of Piauí with a title, a subtitle and sources, `extra`'s parameters over these.
fn panel(s: &mut Session, extra: Value) -> Result<Value, String> {
    let mut p = json!({"profile": "a4-portrait", "style": "panel", "extent": PIAUI, "title": "PIAUÍ",
        "subtitle": "Vegetation trend, 2001–2025", "sources": ["Boundaries: IBGE 2022"]});
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    s.run("layout.publication", &p)
}

fn last(s: &Session) -> &Layout {
    s.project.layouts.last().unwrap()
}

fn find<'a>(l: &'a Layout, name: &str) -> Option<&'a Element> {
    l.elements.iter().find(|e| e.name == name)
}

fn named<'a>(l: &'a Layout, name: &str) -> &'a Element {
    find(l, name).unwrap_or_else(|| panic!("no {name}"))
}

fn boxes(l: &Layout) -> Vec<(String, f64, f64, f64, f64)> {
    l.elements.iter().map(|e| (e.name.clone(), e.x, e.y, e.w, e.h)).collect()
}

fn three_figures() -> Value {
    json!([{"value": "{count:Countries}", "label": "countries"}, {"value": "12 %", "label": "of the area"},
        {"value": "2001–2025"}])
}

/// The figures are inside the panel, one under another, and the locator and the legend under the
/// last of them.
fn assert_stacked(l: &Layout, n: usize, what: &str) {
    let panel = named(l, "Panel Box");
    let mut bottom = find(l, "Panel Rule").map_or(panel.y, |r| r.y);
    for i in 1..=n {
        let value = named(l, &format!("Figure {i} Value"));
        for e in [Some(value), find(l, &format!("Figure {i} Label"))].into_iter().flatten() {
            assert!(e.x >= panel.x - 1e-6 && e.x + e.w <= panel.x + panel.w + 1e-6, "{what}: {} leaves the panel", e.name);
            assert!(e.y >= bottom - 1e-6, "{what}: {} overlaps what is over it", e.name);
            bottom = e.y + e.h;
        }
    }
    assert!(bottom <= panel.y + panel.h + 1e-6, "{what}: the figures pass the panel's foot");
    assert!(find(l, &format!("Figure {} Value", n + 1)).is_none(), "{what}");
    let (mut locators, mut legends) = (0, 0);
    for e in &l.elements {
        let (locator, legend) = (e.name.starts_with("Locator"), e.name.starts_with("Legend"));
        if locator || legend {
            assert!(e.y >= bottom - 1e-6, "{what}: {} over the figures", e.name);
        }
        (locators, legends) = (locators + usize::from(locator), legends + usize::from(legend));
    }
    assert!(locators > 0 && legends > 0, "{what}: {locators} locators, {legends} legends");
}

#[test]
fn a_panel_stacks_its_headline_figures_over_the_locator_and_the_legend() {
    let mut s = world();
    for n in 1..=3 {
        let all = three_figures();
        panel(&mut s, json!({"figures": all.as_array().unwrap()[..n]})).unwrap();
        let l = last(&s);
        assert_stacked(l, n, &format!("{n} figures"));
        let ElementKind::Text { text, bold, size, wrap, .. } = &named(l, "Figure 1 Value").kind else { panic!() };
        assert_eq!((text.as_str(), *bold, *size, *wrap), ("{count:Countries}", true, 18.0, true));
    }
    assert!(find(last(&s), "Figure 3 Label").is_none());
}

#[test]
fn a_panel_with_no_title_puts_the_first_figure_at_its_top() {
    let mut s = world();
    panel(&mut s, json!({"title": null, "subtitle": null, "figures": [{"value": "12 %"}]})).unwrap();
    let l = last(&s);
    assert!(find(l, "Panel Rule").is_none());
    assert!((named(l, "Figure 1 Value").y - named(l, "Panel Box").y - 3.0).abs() < 1e-6);
    assert_stacked(l, 1, "no title");
}

#[test]
fn a_panels_figures_fit_every_page_size() {
    for profile in crate::cmd::layout::PUBLICATION_PROFILES.iter().filter(|p| p.sheet) {
        let mut s = world();
        panel(&mut s, json!({"profile": profile.id, "figures": three_figures()})).unwrap_or_else(|e| panic!("{}: {e}", profile.id));
        assert_stacked(last(&s), 3, profile.id);
    }
}

#[test]
fn a_panel_cut_to_a_low_area_moves_its_figures_with_it() {
    // Areas a bare panel is cut to, which the figures' room must not leave uncut.
    for south in [-20.0, -24.0, -28.0] {
        let mut s = world();
        panel(&mut s, json!({"extent": [-60.0, south, -30.0, -2.0], "figures": three_figures()})).unwrap();
        let l = last(&s);
        // A4's margin and padding put an uncut panel's top at 15 mm.
        assert!(named(l, "Panel Box").y > 16.0, "{south}: {}", named(l, "Panel Box").y);
        assert_stacked(l, 3, &format!("cut to {south}"));
    }
}

#[test]
fn a_panel_with_no_figures_is_laid_out_as_before() {
    let mut s = world();
    panel(&mut s, json!({})).unwrap();
    let bare = boxes(last(&s));
    for figures in [json!(null), json!([])] {
        panel(&mut s, json!({"figures": figures})).unwrap();
        assert_eq!(boxes(last(&s)), bare, "{figures}");
    }
}

#[test]
fn a_panels_texts_that_leave_no_room_name_the_figures() {
    let long = "a figure's value that takes many lines of its column ".repeat(30);
    let mut s = world();
    let e = panel(&mut s, json!({"figures": [{"value": long}]})).unwrap_err();
    assert!(e.contains("no room") && e.contains("the figures"), "{e}");
    assert!(s.project.layouts.is_empty());
}

#[test]
fn a_panels_figures_are_checked_as_an_edge_sheets() {
    let mut s = world();
    let one = json!({"value": "1"});
    for bad in [json!([one, one, one, one]), json!([{"value": ""}]), json!("12 %")] {
        let e = panel(&mut s, json!({"figures": bad})).unwrap_err();
        assert!(e.contains("figures"), "{bad}: {e}");
    }
    assert!(s.project.layouts.is_empty());
}

#[test]
fn a_panel_with_figures_is_one_undo_step() {
    let mut s = world();
    panel(&mut s, json!({"figures": three_figures()})).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    assert!(s.project.layouts.is_empty());
}
