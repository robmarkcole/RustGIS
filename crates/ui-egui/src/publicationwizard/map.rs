//! The wizard's map step.
use super::*;
use crate::dialogs::MapOwner;
use rustgis_engine::map::Layer;

/// Which area the map frame shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Area {
    /// The view, left to `layout.publication`.
    View,
    /// The features a layer shows (hidden or not), in the map it was picked in. The layer by its
    /// [`Layer::instance_id`](rustgis_engine::map::Layer::instance_id): Undo hands its map-local
    /// id to the next layer added.
    Layer { map: MapOwner, layer: u64 },
    /// The selected features of the map it was picked in.
    Selection { map: MapOwner },
}

/// The name `layout.publication` gives when none is: data, not translated.
const DEFAULT_NAME: &str = "Publication";

/// What a side with no length (a point, a straight line) is widened to, in degrees; on the ±180 or
/// ±90 edge half of it is cut off again.
const MIN_SIDE: f64 = 0.01;

/// What a pick names in the session now.
enum Held<'a> {
    View,
    Layer(&'a Layer),
    Selection,
}

/// What `area` names now, or why it no longer holds: the one check the step, the review and Create
/// share, so a stale pick reads the same in the three. It reads no places, as it runs every frame.
fn held<'a>(s: &'a Session, area: &Area) -> Result<Held<'a>, &'static str> {
    let map = s.map();
    match area {
        Area::View => Ok(Held::View),
        Area::Layer { map: owner, .. } | Area::Selection { map: owner } if !owner.holds(s) => {
            Err(tl!("The map was switched or renamed since the map extent was picked: pick it again."))
        }
        Area::Layer { layer, .. } => map
            .layers
            .iter()
            .find(|l| l.instance_id() == *layer)
            .map(Held::Layer)
            .ok_or_else(|| tl!("The layer picked is no longer in the map: pick the map extent again.")),
        Area::Selection { .. } if map.selection_count() == 0 => Err(tl!("Nothing is selected: pick the map extent again.")),
        Area::Selection { .. } => Ok(Held::Selection),
    }
}

/// The map step: the area the map frame shows and the layout's name.
pub(super) fn map_step(ui: &mut egui::Ui, s: &Session, d: &mut Draft) {
    let map = s.map();
    // In the map's layer order, top first, by what costs nothing a frame: whether it has rows, a
    // raster or a source. Its places are read at Create ([`Layer::extent`] walks every
    // longitude/latitude row each time), where one with none to frame is refused.
    let framable = || map.layers.iter().filter(|l| !l.dataset.features.is_empty() || l.raster.is_some() || l.source.is_some());
    egui::Grid::new("publication_wizard_map").num_columns(2).spacing(egui::vec2(8.0, 8.0)).show(ui, |ui| {
        // A row for each choice, so the label sits beside the first one.
        ui.label(tl!("Map extent"));
        if ui.radio(d.area == Area::View, tl!("View")).on_hover_text(tl!("What the map view shows.")).clicked() {
            d.area = Area::View;
        }
        ui.end_row();
        ui.label("");
        // A pick that no longer holds marks nothing.
        let held = held(s, &d.area);
        ui.horizontal(|ui| {
            let picked = match held {
                Ok(Held::Layer(l)) => Some(l),
                _ => None,
            };
            let top = framable().next().map(Layer::instance_id);
            let radio = ui.add_enabled(top.is_some(), egui::RadioButton::new(picked.is_some(), tl!("Layer")));
            if radio.clicked()
                && let Some(layer) = picked.map(Layer::instance_id).or(top)
            {
                d.area = Area::Layer { map: MapOwner::of(s), layer };
            }
            if let Some(picked) = picked {
                egui::ComboBox::from_id_salt("publication_wizard_map_layer")
                    .selected_text(picked.name.as_str())
                    .width(ui.available_width().min(180.0))
                    .truncate()
                    .show_ui(ui, |ui| {
                        for l in framable() {
                            if ui.selectable_label(l.instance_id() == picked.instance_id(), l.name.as_str()).clicked() {
                                d.area = Area::Layer { map: MapOwner::of(s), layer: l.instance_id() };
                            }
                        }
                    })
                    .response
                    .labelled_by(radio.id);
            }
        });
        ui.end_row();
        ui.label("");
        let selection = egui::RadioButton::new(matches!(held, Ok(Held::Selection)), tl!("Selection"));
        if ui
            .add_enabled(map.selection_count() > 0, selection)
            .on_hover_text(tl!("The selected features."))
            .on_disabled_hover_text(tl!("Nothing is selected in the map."))
            .clicked()
        {
            d.area = Area::Selection { map: MapOwner::of(s) };
        }
        ui.end_row();
        let label = ui.label(tl!("Name"));
        ui.add(egui::TextEdit::singleline(&mut d.name).hint_text(DEFAULT_NAME).desired_width(field_width(ui))).labelled_by(label.id);
        ui.end_row();
    });
}

/// How the review names the layout: as typed, or as the command names one given no name, saying
/// when it numbers it because a layout has that name.
pub(super) fn name_label(s: &Session, name: &str) -> String {
    let name = Some(name.trim()).filter(|n| !n.is_empty()).unwrap_or(DEFAULT_NAME);
    if s.project.layouts.iter().any(|l| l.name.eq_ignore_ascii_case(name)) {
        i18n::fmt("{name} (numbered: a layout has that name)", &[("name", name)])
    } else {
        name.to_string()
    }
}

/// How the review names the area.
pub(super) fn area_label(s: &Session, area: &Area) -> String {
    match held(s, area) {
        Ok(Held::View) => tl!("The view").to_string(),
        Ok(Held::Layer(l)) => i18n::fmt("Layer: {layer}", &[("layer", &l.name)]),
        Ok(Held::Selection) => tl!("Selected features").to_string(),
        Err(_) => tl!("(none)").to_string(),
    }
}

/// `e` as `layout.publication` takes it: cut to ±180 and ±90, a side of no length widened to
/// [`MIN_SIDE`]; none when nothing is left to frame.
fn framed(e: Extent) -> Option<[f64; 4]> {
    if e.is_empty() {
        return None;
    }
    let side = |min: f64, max: f64, limit: f64| {
        let pad = if max > min { 0.0 } else { MIN_SIDE / 2.0 };
        let (min, max) = ((min - pad).max(-limit), (max + pad).min(limit));
        (min < max).then_some((min, max))
    };
    let ((west, east), (south, north)) = (side(e.min_x, e.max_x, 180.0)?, side(e.min_y, e.max_y, 90.0)?);
    Some([west, south, east, north])
}

/// The `extent` to send for `area`, none for the view; a pick that no longer holds is an error
/// rather than another area framed. Read once, at Create, on the UI thread where
/// `layout.publication` runs next: it may walk the whole layer, or the whole selection.
pub(super) fn area_extent(s: &Session, area: &Area) -> Result<Option<[f64; 4]>, String> {
    let extent = match held(s, area)? {
        Held::View => return Ok(None),
        Held::Layer(l) => l.shown_extent(),
        Held::Selection => s.map().selection_extent(),
    };
    framed(extent).map(Some).ok_or_else(|| tl!("There is nothing to frame there: pick another map extent.").to_string())
}

#[cfg(test)]
mod tests {
    use super::harness::*;
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT, Queryable};
    use rustgis_engine::data::{Dataset, GeometryType};

    fn close(got: Option<[f64; 4]>, want: [f64; 4]) {
        let got = got.expect("an extent");
        assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-9), "{got:?} is not {want:?}");
    }

    /// A side of no length (a point, a straight line) is widened to 0.01°, the rest is left as it
    /// is; what lies past ±180 and ±90 is cut; nothing left to frame is none.
    #[test]
    fn an_extent_is_widened_and_cut_to_what_the_command_takes() {
        close(framed(Extent::new(10.0, 20.0, 10.0, 20.0)), [9.995, 19.995, 10.005, 20.005]);
        close(framed(Extent::new(10.0, 20.0, 12.0, 20.0)), [10.0, 19.995, 12.0, 20.005]);
        close(framed(Extent::new(10.0, 20.0, 10.0, 22.0)), [9.995, 20.0, 10.005, 22.0]);
        close(framed(Extent::new(180.0, 90.0, 180.0, 90.0)), [179.995, 89.995, 180.0, 90.0]);
        close(framed(Extent::new(-190.0, -95.0, 200.0, 91.0)), [-180.0, -90.0, 180.0, 90.0]);
        assert_eq!(framed(Extent::EMPTY), None);
        assert_eq!(framed(Extent::new(190.0, 0.0, 200.0, 1.0)), None);
    }

    /// The view sends no extent; a layer sends the extent of the features it shows; the
    /// selection, that of the selected features.
    #[test]
    fn each_area_sends_its_own_extent() {
        let mut app = app();
        let map = MapOwner::of(&app.session);
        assert_eq!(area_extent(&app.session, &Area::View), Ok(None));
        app.run("layer.definitionQuery", json!({"layer": "Cities", "where": "pop_max > 10000000"})).unwrap();
        let (id, shown, all) = {
            let l = app.session.map().layers.iter().find(|l| l.name == "Cities").unwrap();
            (l.instance_id(), l.shown_extent(), l.extent())
        };
        assert_ne!(shown, all, "the query narrows it");
        assert_eq!(area_extent(&app.session, &Area::Layer { map: map.clone(), layer: id }), Ok(framed(shown)));
        app.run("select.byAttributes", json!({"layer": "Cities", "where": "name = 'Tokyo'"})).unwrap();
        let selected = app.session.map().selection_extent();
        let got = area_extent(&app.session, &Area::Selection { map }).unwrap();
        assert_eq!(got, framed(selected));
        assert!(got.is_some_and(|[w, s, e, n]| w < e && s < n), "one point, framed: {got:?}");
    }

    /// The step starts on the view; Layer picks one from the list; Selection is off with nothing
    /// selected.
    #[test]
    fn the_step_picks_the_view_a_layer_or_the_selection() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Map;
        let mut h = harness(app);
        let area = |h: &egui_kittest::Harness<'static, RustGisApp>| h.state().dialogs.publication.as_ref().unwrap().area.clone();
        assert_eq!(area(&h), Area::View);
        assert!(h.get_by_role_and_label(Role::RadioButton, "Selection").accesskit_node().is_disabled());
        h.get_by_role_and_label(Role::RadioButton, "Layer").click();
        h.run_steps(4);
        let top = h.state().session.map().layers.first().unwrap().instance_id();
        assert_eq!(h.state().session.map().layers[0].name, "Cities", "the layer drawn on top");
        assert!(matches!(area(&h), Area::Layer { layer, .. } if layer == top), "{:?}", area(&h));
        h.state_mut().run("select.byAttributes", json!({"layer": "Cities", "where": "name = 'Tokyo'"})).unwrap();
        h.run_steps(2);
        h.get_by_role_and_label(Role::RadioButton, "Selection").click();
        h.run_steps(4);
        assert!(matches!(area(&h), Area::Selection { .. }), "{:?}", area(&h));
    }

    /// A long layer name is cut to the list's width, inside the window at its smallest.
    #[test]
    fn the_layer_list_stays_in_the_window() {
        let mut app = app();
        open(&mut app);
        let long = "A layer with a very long name ".repeat(6);
        app.run("layer.rename", json!({"layer": "Cities", "name": long})).unwrap();
        let map = MapOwner::of(&app.session);
        let instance = app.session.map().layers[0].instance_id();
        let d = app.dialogs.publication.as_mut().unwrap();
        (d.area, d.step) = (Area::Layer { map, layer: instance }, Step::Map);
        let h = harness_sized(app, egui::vec2(420.0, 600.0));
        let list = h.get_by_role(Role::ComboBox).rect();
        assert!(list.right() <= 420.0, "the list runs to {}", list.right());
    }

    /// The review names what Create will make: a selection cleared since reads as none, and a name
    /// another layout has (the default's too, case aside) is said to be numbered.
    #[test]
    fn the_review_says_a_cleared_selection_and_a_taken_name() {
        let mut app = app();
        open(&mut app);
        app.run("select.byAttributes", json!({"layer": "Cities", "where": "name = 'Tokyo'"})).unwrap();
        let d = app.dialogs.publication.as_mut().unwrap();
        (d.area, d.step) = (Area::Selection { map: MapOwner::of(&app.session) }, Step::Review);
        let mut h = harness(app);
        assert!(h.query_by_label("Selected features").is_some());
        assert!(h.query_by_label("Publication").is_some());
        h.state_mut().run("select.clear", Value::Null).unwrap();
        h.state_mut().run("layout.new", json!({"name": "publication"})).unwrap();
        h.state_mut().dialogs.open = Some("publicationWizard");
        h.run_steps(4);
        assert!(h.query_by_label("Selected features").is_none());
        assert!(h.query_by_label("Publication (numbered: a layout has that name)").is_some());
        h.state_mut().dialogs.publication.as_mut().unwrap().name = " Publication ".into();
        h.run_steps(4);
        assert!(h.query_by_label("Publication (numbered: a layout has that name)").is_some());
    }

    /// A layer is offered for having rows, its places left unread until Create: one with none to
    /// frame is refused there; a layer with no rows isn't offered.
    #[test]
    fn a_layer_is_offered_by_its_rows_and_its_places_read_at_create() {
        let mut app = app();
        open(&mut app);
        let mut unplaced = Dataset::new("No places", GeometryType::Point, vec![]);
        unplaced.push(None, vec![]);
        app.session.map_mut().add_dataset(unplaced);
        app.session.map_mut().add_dataset(Dataset::new("No rows", GeometryType::Point, vec![]));
        app.dialogs.publication.as_mut().unwrap().step = Step::Map;
        let mut h = harness(app);
        h.get_by_role_and_label(Role::RadioButton, "Layer").click();
        h.run_steps(4);
        let state = h.state();
        let area = state.dialogs.publication.as_ref().unwrap().area.clone();
        assert_eq!(area_label(&state.session, &area), "Layer: No places");
        assert_eq!(area_extent(&state.session, &area), Err("There is nothing to frame there: pick another map extent.".to_string()));
    }

    /// A pick that no longer holds reads the same everywhere: no choice marked on the step,
    /// "(none)" in the review, and Create says why instead of framing something else.
    #[test]
    fn a_pick_that_no_longer_holds_reads_the_same_everywhere() {
        const SWITCHED: &str = "The map was switched or renamed since the map extent was picked: pick it again.";
        const GONE: &str = "The layer picked is no longer in the map: pick the map extent again.";
        const CLEARED: &str = "Nothing is selected: pick the map extent again.";
        type Change = fn(&mut RustGisApp);
        fn run(app: &mut RustGisApp, id: &str, p: Value) {
            app.run(id, p).unwrap();
        }
        let cases: [(&str, bool, Change, &str); 9] = [
            ("another map, layer", true, |a| run(a, "map.new", json!({})), SWITCHED),
            ("another map, selection", false, |a| run(a, "map.new", json!({})), SWITCHED),
            ("map renamed, layer", true, |a| run(a, "map.rename", json!({"name": "Renamed"})), SWITCHED),
            ("map renamed, selection", false, |a| run(a, "map.rename", json!({"name": "Renamed"})), SWITCHED),
            ("project replaced, layer", true, |a| replace_project(a), SWITCHED),
            ("project replaced, selection", false, |a| replace_project(a), SWITCHED),
            ("layer removed", true, |a| run(a, "layer.remove", json!({"layer": "Rivers copy"})), GONE),
            (
                "paste undone, its id reused",
                true,
                |a| {
                    run(a, "edit.undo", Value::Null);
                    run(a, "layer.copy", json!({"layer": "Lakes"}));
                    run(a, "layer.paste", json!({}));
                },
                GONE,
            ),
            ("selection cleared", false, |a| run(a, "select.clear", Value::Null), CLEARED),
        ];
        for (case, layer, change, why) in cases {
            let mut app = app();
            run(&mut app, "select.byAttributes", json!({"layer": "Cities", "where": "name = 'Tokyo'"}));
            run(&mut app, "layer.copy", json!({"layer": "Rivers"}));
            run(&mut app, "layer.paste", json!({}));
            let map = MapOwner::of(&app.session);
            let area = if layer { Area::Layer { map, layer: app.session.map().layers[0].instance_id() } } else { Area::Selection { map } };
            change(&mut app);
            assert_eq!(area_label(&app.session, &area), "(none)", "{case}");
            assert_eq!(area_extent(&app.session, &area), Err(why.to_string()), "{case}");
            open(&mut app);
            let d = app.dialogs.publication.as_mut().unwrap();
            (d.area, d.step) = (area, Step::Map);
            let h = harness(app);
            for name in ["View", "Layer", "Selection"] {
                let radio = h.get_by_role_and_label(Role::RadioButton, name);
                assert_ne!(radio.accesskit_node().toggled(), Some(egui::accesskit::Toggled::True), "{case}: {name}");
            }
            assert!(h.query_by_role(Role::ComboBox).is_none(), "{case}: no layer list");
        }
    }

    /// A project opened over this one, its map named as the old one was.
    fn replace_project(app: &mut RustGisApp) {
        let name = app.session.map().name.clone();
        app.run("project.new", json!({})).unwrap();
        app.run("map.rename", json!({"name": name})).unwrap();
    }

    /// Create frames the layer picked and names the layout as typed; a figure takes both too.
    #[test]
    fn create_frames_the_area_and_names_the_layout() {
        for size in ["a4", "figure-1col"] {
            let mut app = app();
            open(&mut app);
            let map = MapOwner::of(&app.session);
            app.run("select.byAttributes", json!({"layer": "Cities", "where": "name = 'Tokyo'"})).unwrap();
            let [w, s, e, n] = framed(app.session.map().selection_extent()).unwrap();
            {
                let d = app.dialogs.publication.as_mut().unwrap();
                d.size = size.into();
                d.area = Area::Selection { map };
                d.name = "  Tokyo sheet ".into();
                d.step = Step::Review;
            }
            let mut h = harness(app);
            click(&mut h, "Create");
            let l = h.state().session.project.layouts.last().unwrap();
            assert_eq!(l.name, "Tokyo sheet", "{size}");
            let center = l.elements.iter().find_map(|el| match &el.kind {
                rustgis_engine::map::layout::ElementKind::MapFrame { center, .. } if el.name == "Map Frame" => Some(*center),
                _ => None,
            });
            let c = center.expect("the map frame");
            assert!(c.x > w - 0.01 && c.x < e + 0.01 && c.y > s - 0.01 && c.y < n + 0.01, "{size}: {c:?}");
        }
    }

    /// An area gone at Create is said, and the wizard stays with nothing made.
    #[test]
    fn create_with_the_area_gone_is_said_and_makes_nothing() {
        let mut app = app();
        open(&mut app);
        let map = MapOwner::of(&app.session);
        {
            let d = app.dialogs.publication.as_mut().unwrap();
            d.area = Area::Selection { map };
            d.step = Step::Review;
        }
        let before = app.session.project.layouts.len();
        let mut h = harness(app);
        let said = h.state().session.notice_mark();
        click(&mut h, "Create");
        let app = h.state();
        assert_eq!(app.session.project.layouts.len(), before);
        assert_eq!(app.session.notice_mark(), said + 1);
        assert_eq!(app.dialogs.open, Some("publicationWizard"));
    }

    /// The name goes only when typed, for a sheet and for a figure.
    #[test]
    fn the_name_is_sent_only_when_typed() {
        let app = app();
        let mut d = Draft::new(&app.session);
        d.name = "   ".into();
        assert!(d.params().get("name").is_none());
        d.name = "Rivers".into();
        assert_eq!(d.params()["name"], "Rivers");
        d.size = "figure-1col".into();
        assert_eq!(d.params(), json!({"profile": "figure-1col", "name": "Rivers"}));
    }
}
