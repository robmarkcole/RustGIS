//! The wizard's elements step: what is drawn around the map, and in which mode.
use super::arrange::{LOCATOR, NORTH_ARROW, SCALE_BAR, surround_labels};
use super::*;
use rustgis_engine::cmd::layout::{publication_style_cards, publication_style_has_column};
use rustgis_engine::map::layout::colorbar_layer;

/// The locator asked for; `Default` leaves it to `layout.publication`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LocatorChoice {
    #[default]
    Default,
    Globe,
    Inset,
    None,
}

/// When the north arrow is drawn; `Auto` leaves it to the extent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NorthChoice {
    #[default]
    Auto,
    Always,
    None,
}

/// What the step asks. A box the sheet can't use keeps its tick, and isn't sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Elements {
    pub locator: LocatorChoice,
    pub north: NorthChoice,
    pub legend: bool,
    /// The legend's box even where the map under it is plain.
    pub legend_box: bool,
    /// The context layers listed in the legend.
    pub context: bool,
    pub colorbar: bool,
    pub scale_bar: bool,
    /// Whether the map outside the extent may be veiled for the legend's room.
    pub veil: bool,
    /// A decimal comma rather than a point.
    pub comma: bool,
}

impl Default for Elements {
    fn default() -> Self {
        Self {
            locator: LocatorChoice::Default,
            north: NorthChoice::Auto,
            legend: true,
            legend_box: false,
            context: false,
            colorbar: true,
            scale_bar: true,
            veil: true,
            comma: false,
        }
    }
}

/// A control of the step a sheet may have no use for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Control {
    LegendBox,
    Context,
    Colorbar,
    ScaleBar,
    Veil,
}

/// What `layout.publication` may put over or beside the map, as far as the wizard can tell
/// without placing it: the one account of it that the step, what Create sends, the placement and
/// the review share.
impl Draft {
    /// Whether the style keeps the locator and legend beside the map; a figure takes no style.
    pub(super) fn column(&self) -> bool {
        !self.figure() && publication_style_has_column(&self.style)
    }

    /// Whether the style sets its surrounds on cards; a figure takes no style.
    fn cards(&self) -> bool {
        !self.figure() && publication_style_cards(&self.style)
    }

    /// Whether the map frame shows the globe seen whole: on the globe, with no extent sent.
    fn whole_globe(&self, s: &Session) -> bool {
        s.map().projection.is_globe() && self.area == Area::View
    }

    /// Whether the command draws a locator none is asked for: a sheet's, but of a whole globe.
    fn default_locator(&self, s: &Session) -> bool {
        !self.figure() && !self.whole_globe(s)
    }

    /// Whether a locator is drawn: the one asked for, or the command's default.
    pub(super) fn draws_locator(&self, s: &Session) -> bool {
        match self.elements.locator {
            LocatorChoice::Default => self.default_locator(s),
            LocatorChoice::None => false,
            LocatorChoice::Globe | LocatorChoice::Inset => true,
        }
    }

    /// Whether a north arrow may be drawn. Left to the extent, a whole globe has none; any other
    /// extent's span is known only to the command.
    fn draws_arrow(&self, s: &Session) -> bool {
        match self.elements.north {
            NorthChoice::Auto => !self.whole_globe(s),
            NorthChoice::Always => true,
            NorthChoice::None => false,
        }
    }

    fn draws_colorbar(&self, s: &Session) -> bool {
        self.elements.colorbar && self.usable(Control::Colorbar, s)
    }

    fn draws_scale_bar(&self, s: &Session) -> bool {
        self.elements.scale_bar && self.usable(Control::ScaleBar, s)
    }

    /// Whether the legend's place is taken, by its rows or by the colorbar boxed there.
    fn draws_legend_place(&self, s: &Session) -> bool {
        self.elements.legend || self.draws_colorbar(s)
    }

    /// Whether the surround at `at` (a place in `Homes`) is drawn.
    pub(super) fn draws(&self, at: usize, s: &Session) -> bool {
        match at {
            LOCATOR => self.draws_locator(s),
            NORTH_ARROW => self.draws_arrow(s),
            SCALE_BAR => self.draws_scale_bar(s),
            _ => self.draws_legend_place(s),
        }
    }

    /// Whether this sheet has a use for `control`: one without it is off in the step and isn't sent.
    pub(super) fn usable(&self, control: Control, s: &Session) -> bool {
        match control {
            Control::LegendBox => !self.column() && !self.cards() && self.draws_legend_place(s),
            Control::Context => self.elements.legend,
            // A raster still reading has no scale to show yet. With pixels, [`colorbar_layer`]
            // only gives a raster with one.
            Control::Colorbar => colorbar_layer(s.map(), None).is_some_and(|l| l.raster.as_ref().is_some_and(|r| r.data.is_some())),
            Control::ScaleBar => !self.whole_globe(s),
            // The command veils only what is under surrounds over the map.
            Control::Veil => !self.column() && self.draws_legend_place(s),
        }
    }

    /// What Create sends: [`Draft::params`] and the elements step's answers, a choice left to the
    /// command not among them.
    pub fn sent(&self, s: &Session) -> Value {
        let e = &self.elements;
        let mut p = self.params();
        let locator = match e.locator {
            LocatorChoice::Default => None,
            LocatorChoice::Globe => Some("globe"),
            LocatorChoice::Inset => Some("inset"),
            LocatorChoice::None => Some("none"),
        };
        let north = match e.north {
            NorthChoice::Auto => None,
            NorthChoice::Always => Some("always"),
            NorthChoice::None => Some("none"),
        };
        for (key, value) in [
            ("locator", locator.map(Value::from)),
            ("northArrow", north.map(Value::from)),
            ("legend", (!e.legend).then(|| "none".into())),
            ("legendBox", (e.legend_box && self.usable(Control::LegendBox, s)).then(|| "always".into())),
            ("contextInLegend", (e.context && self.usable(Control::Context, s)).then(|| true.into())),
            ("colorbar", (!e.colorbar && self.usable(Control::Colorbar, s)).then(|| "none".into())),
            ("scaleBar", (!e.scale_bar && self.usable(Control::ScaleBar, s)).then(|| "none".into())),
            ("veil", (!e.veil && self.usable(Control::Veil, s)).then(|| false.into())),
            ("decimal", e.comma.then(|| ",".into())),
        ] {
            if let Some(value) = value {
                p[key] = value;
            }
        }
        p
    }
}

/// Why the colorbar's box is off: [`colorbar_layer`] also gives a raster whose pixels, and so
/// colour scale, aren't here, still on their way or failed.
fn colorbar_off_reason(s: &Session) -> &'static str {
    match colorbar_layer(s.map(), None).and_then(|l| l.raster.as_ref()) {
        Some(r) if r.error.is_some() => tl!("The top raster couldn't be read."),
        Some(_) => tl!("The top raster is still being read."),
        None => tl!("The map has no raster with a colour scale."),
    }
}

/// What is on, a line each, as the review lists it.
pub(super) fn elements_value(s: &Session, d: &Draft) -> String {
    let e = &d.elements;
    let mut on: Vec<String> = Vec::new();
    if d.draws_locator(s) {
        let which = match e.locator {
            LocatorChoice::Globe => tl_raw!("Globe"),
            LocatorChoice::Inset => tl_raw!("Inset"),
            _ => tl_raw!("Default (globe)"),
        };
        on.push(i18n::fmt("{surround}: {choice}", &[("surround", surround_labels()[LOCATOR]), ("choice", which)]));
    }
    if d.draws_arrow(s) {
        // Left to the extent, the command leaves it out from a span only Create measures.
        let when = if e.north == NorthChoice::Always { tl_raw!("Always") } else { tl_raw!("Auto (none from 20°)") };
        on.push(i18n::fmt("{surround}: {choice}", &[("surround", surround_labels()[NORTH_ARROW]), ("choice", when)]));
    }
    let mut push_if = |is: bool, line: &str| {
        if is {
            on.push(line.to_string());
        }
    };
    push_if(e.legend, tl!("Legend"));
    push_if(e.legend_box && d.usable(Control::LegendBox, s), tl!("Always on a box"));
    push_if(e.context && d.usable(Control::Context, s), tl!("Context layers listed"));
    push_if(d.draws_colorbar(s), tl!("Colorbar"));
    push_if(d.draws_scale_bar(s), tl!("Scale bar"));
    push_if(!e.veil && d.usable(Control::Veil, s), tl!("No veil"));
    push_if(true, if e.comma { tl!("Decimal comma") } else { tl!("Decimal point") });
    on.join("\n")
}

/// The elements step: what is drawn around the map, and how.
pub(super) fn elements_step(ui: &mut egui::Ui, s: &Session, d: &mut Draft) {
    let default_locator = if d.default_locator(s) { tl!("Default: Globe") } else { tl!("Default: None") };
    ui.label(tl!("Locator"));
    ui.horizontal_wrapped(|ui| {
        let choice = &mut d.elements.locator;
        ui.radio_value(choice, LocatorChoice::Default, default_locator);
        ui.radio_value(choice, LocatorChoice::Globe, tl!("Globe"));
        ui.radio_value(choice, LocatorChoice::Inset, tl!("Inset"));
        ui.radio_value(choice, LocatorChoice::None, tl!("None"));
    });
    ui.label(tl!("North arrow"));
    ui.horizontal_wrapped(|ui| {
        let choice = &mut d.elements.north;
        ui.radio_value(choice, NorthChoice::Auto, tl!("Auto")).on_hover_text(tl!("None when the extent spans 20° or more."));
        ui.radio_value(choice, NorthChoice::Always, tl!("Always"));
        ui.radio_value(choice, NorthChoice::None, tl!("None"));
    });
    ui.add_space(4.0);
    // A box the sheet has no use for is off, its tick kept for one that has.
    let tick = |ui: &mut egui::Ui, usable: bool, on: &mut bool, label: &str, why: &str| {
        ui.add_enabled(usable, egui::Checkbox::new(on, label)).on_disabled_hover_text(why)
    };
    ui.checkbox(&mut d.elements.legend, tl!("Legend"));
    ui.indent("publication_wizard_legend", |ui| {
        let own_box = d.column() || d.cards();
        let usable = d.usable(Control::LegendBox, s);
        let own_note = tl!("In this style the legend has a box of its own.");
        let why = if own_box { own_note } else { tl!("There is no legend or colorbar to box.") };
        tick(ui, usable, &mut d.elements.legend_box, tl!("Always on a box"), why);
        let usable = d.usable(Control::Context, s);
        tick(ui, usable, &mut d.elements.context, tl!("List the context layers"), tl!("There is no legend to list them in."));
        if own_box {
            ui.label(egui::RichText::new(own_note).small().weak());
        }
    });
    let usable = d.usable(Control::Colorbar, s);
    let why = colorbar_off_reason(s);
    tick(ui, usable, &mut d.elements.colorbar, tl!("Colorbar"), why)
        .on_hover_text(tl!("Drawn when the map's top raster has a colour scale."));
    let usable = d.usable(Control::ScaleBar, s);
    let why = tl!("A map of the whole globe draws no scale bar.");
    tick(ui, usable, &mut d.elements.scale_bar, tl!("Scale bar"), why);
    if !usable {
        ui.label(egui::RichText::new(why).small().weak());
    }
    let usable = d.usable(Control::Veil, s);
    let why = if d.column() { tl!("In this style the map isn't veiled.") } else { tl!("There is no legend or colorbar to make room for.") };
    tick(ui, usable, &mut d.elements.veil, tl!("Veil the map outside the extent if the legend needs room"), why);
    ui.add_space(4.0);
    ui.label(tl!("Decimal mark"));
    ui.horizontal_wrapped(|ui| {
        ui.radio_value(&mut d.elements.comma, false, tl!("Point (1.5)"));
        ui.radio_value(&mut d.elements.comma, true, tl!("Comma (1,5)"));
    });
}

#[cfg(test)]
mod tests {
    use super::super::harness::*;
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT, Queryable};
    use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster};
    use rustgis_engine::map::layout::ElementKind;
    use rustgis_engine::map::{Projection, RasterLayer};

    type Wizard = egui_kittest::Harness<'static, RustGisApp>;

    /// The keys the step may send.
    const KEYS: [&str; 9] = ["locator", "northArrow", "legend", "legendBox", "contextInLegend", "colorbar", "scaleBar", "veil", "decimal"];

    /// What the step adds to the command's parameters, by key.
    fn added(s: &Session, d: &Draft) -> Value {
        let sent = d.sent(s);
        let kept = KEYS.iter().filter_map(|k| sent.get(*k).map(|v| (k.to_string(), v.clone())));
        Value::Object(kept.collect())
    }

    fn globe(app: &mut RustGisApp) {
        app.session.map_mut().projection = Projection::Globe;
        assert!(app.session.map().projection.is_globe());
    }

    /// The elements step of a wizard prepared by `prepare`.
    fn step_of(prepare: impl FnOnce(&mut RustGisApp)) -> Wizard {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Elements;
        prepare(&mut app);
        harness(app)
    }

    fn draft(h: &Wizard) -> &Draft {
        h.state().dialogs.publication.as_ref().expect("the wizard is open")
    }

    fn ticked_box<'a>(h: &'a Wizard, label: &'a str) -> egui_kittest::Node<'a> {
        h.get_by_role_and_label(Role::CheckBox, label)
    }

    fn off(h: &Wizard, label: &str) -> bool {
        ticked_box(h, label).accesskit_node().is_disabled()
    }

    fn click_box(h: &mut Wizard, label: &str) {
        ticked_box(h, label).click();
        h.run_steps(4);
    }

    /// The `nth` radio button called `label`, top to bottom: None is both the locator's and the
    /// north arrow's.
    fn pick(h: &mut Wizard, label: &str, nth: usize) {
        h.get_all_by_role_and_label(Role::RadioButton, label).nth(nth).expect("the radio").click();
        h.run_steps(4);
    }

    /// A new draft sends what it sent before the step.
    #[test]
    fn a_new_draft_adds_nothing() {
        let app = app();
        for size in ["a4", "figure-1col"] {
            let mut d = Draft::new(&app.session);
            d.size = size.into();
            assert_eq!(d.sent(&app.session), d.params(), "{size}");
        }
    }

    /// Each choice sends its own key, on a sheet and on a figure.
    #[test]
    fn each_choice_is_sent() {
        let mut app = app();
        raster(&mut app);
        type Set = fn(&mut Elements);
        let cases: [(Set, Value); 12] = [
            (|e| e.locator = LocatorChoice::Globe, json!({"locator": "globe"})),
            (|e| e.locator = LocatorChoice::Inset, json!({"locator": "inset"})),
            (|e| e.locator = LocatorChoice::None, json!({"locator": "none"})),
            (|e| e.north = NorthChoice::Always, json!({"northArrow": "always"})),
            (|e| e.north = NorthChoice::None, json!({"northArrow": "none"})),
            (|e| e.legend = false, json!({"legend": "none"})),
            (|e| e.legend_box = true, json!({"legendBox": "always"})),
            (|e| e.context = true, json!({"contextInLegend": true})),
            (|e| e.colorbar = false, json!({"colorbar": "none"})),
            (|e| e.scale_bar = false, json!({"scaleBar": "none"})),
            (|e| e.veil = false, json!({"veil": false})),
            (|e| e.comma = true, json!({"decimal": ","})),
        ];
        for size in ["a4", "figure-1col"] {
            for (set, want) in &cases {
                let mut d = Draft::new(&app.session);
                d.size = size.into();
                set(&mut d.elements);
                assert_eq!(&added(&app.session, &d), want, "{size}");
            }
        }
    }

    /// With no legend its context box isn't sent; its box still is while there is a colorbar to
    /// box, and isn't once there is none.
    #[test]
    fn without_a_legend_the_box_follows_the_colorbar() {
        let mut app = app();
        let mut d = Draft::new(&app.session);
        d.elements = Elements { legend: false, legend_box: true, context: true, ..Elements::default() };
        assert_eq!(added(&app.session, &d), json!({"legend": "none"}), "a map of vectors has no colorbar");
        raster(&mut app);
        assert_eq!(added(&app.session, &d), json!({"legend": "none", "legendBox": "always"}), "the colorbar's box");
        app.session.map_mut().layers.retain(|l| l.raster.is_none());
        assert_eq!(added(&app.session, &d), json!({"legend": "none"}), "the raster removed meanwhile");
    }

    /// A style with a column or cards keeps its own box, and one with a column is never veiled; a
    /// figure after such a style has neither.
    #[test]
    fn a_style_with_a_column_or_cards_takes_neither() {
        let app = app();
        for (style, boxed, veiled) in [
            ("classic", true, true),
            ("banner", true, true),
            ("edge", true, true),
            ("panel", false, false),
            ("editorial", false, false),
            ("modular", false, false),
            ("poster", false, true),
        ] {
            let mut d = Draft::new(&app.session);
            d.style = style.into();
            d.elements = Elements { legend_box: true, veil: false, ..Elements::default() };
            let sent = added(&app.session, &d);
            assert_eq!(sent.get("legendBox").is_some(), boxed, "{style}: the box");
            assert_eq!(sent.get("veil").is_some(), veiled, "{style}: the veil");
            d.size = "figure-1col".into();
            assert_eq!(added(&app.session, &d), json!({"legendBox": "always", "veil": false}), "{style}: a figure");
        }
    }

    /// With no raster colour scale to show, the colorbar is off and its answer isn't sent; the tick
    /// is kept, and counts again once there is a raster.
    #[test]
    fn without_a_colour_scale_the_colorbar_is_off_and_not_sent() {
        let mut h = step_of(|app| app.dialogs.publication.as_mut().unwrap().elements.colorbar = false);
        assert!(off(&h, "Colorbar"), "a map of vectors");
        assert_eq!(added(&h.state().session, draft(&h)), json!({}));
        raster(h.state_mut());
        h.run_steps(4);
        assert!(!off(&h, "Colorbar"), "a raster added meanwhile");
        assert_eq!(added(&h.state().session, draft(&h)), json!({"colorbar": "none"}));
    }

    /// The off colorbar says whether the map has no colour scale, or its top raster's isn't read:
    /// still on its way, or failed.
    #[test]
    fn the_off_colorbar_says_why() {
        let mut app = app();
        assert_eq!(colorbar_off_reason(&app.session), "The map has no raster with a colour scale.");
        let id = app.session.map_mut().add_raster("Scene", RasterLayer::unread_in_place("https://example.com/scene.tif"));
        assert_eq!(colorbar_off_reason(&app.session), "The top raster is still being read.");
        app.session.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap().fail("gone");
        assert_eq!(colorbar_off_reason(&app.session), "The top raster couldn't be read.");
    }

    /// With nothing to make room for, the veil isn't sent.
    #[test]
    fn without_a_legend_or_colorbar_the_veil_isnt_sent() {
        let app = app();
        let mut d = Draft::new(&app.session);
        d.elements = Elements { legend: false, veil: false, ..Elements::default() };
        assert_eq!(added(&app.session, &d), json!({"legend": "none"}));
    }

    /// The whole globe in view draws no scale bar, so none is sent; a layer's extent on the same
    /// globe does.
    #[test]
    fn a_whole_globe_sends_no_scale_bar() {
        let mut app = app();
        globe(&mut app);
        let mut d = Draft::new(&app.session);
        d.elements.scale_bar = false;
        assert_eq!(added(&app.session, &d), json!({}));
        let layer = app.session.map().layers[0].instance_id();
        d.area = Area::Layer { map: crate::dialogs::MapOwner::of(&app.session), layer };
        assert_eq!(added(&app.session, &d), json!({"scaleBar": "none"}));
    }

    /// A raster on a colour scale on top of the map.
    fn raster(app: &mut RustGisApp) {
        let values: Vec<f32> = (0..16).map(|v| v as f32).collect();
        let at = GeoTransform { origin_x: -20.0, origin_y: 10.0, pixel_width: 10.0, pixel_height: -5.0 };
        let r = Raster::new("dem", 4, 4, vec![values], at, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        app.session.map_mut().add_raster("Heights", RasterLayer::from_path(r, "dem.tif"));
    }

    /// The default locator is named as what it will be.
    #[test]
    fn the_default_locator_says_what_it_draws() {
        let said = |h: &Wizard, label: &str| h.query_by_role_and_label(Role::RadioButton, label).is_some();
        assert!(said(&step_of(|_| {}), "Default: Globe"), "a sheet");
        let figure = step_of(|app| app.dialogs.publication.as_mut().unwrap().size = "figure-1col".into());
        assert!(said(&figure, "Default: None"), "a figure");
        assert!(said(&step_of(globe), "Default: None"), "a whole globe");
    }

    /// Each control sets its answer.
    #[test]
    fn the_controls_set_the_answers() {
        let mut h = step_of(raster);
        pick(&mut h, "Inset", 0);
        pick(&mut h, "None", 1);
        pick(&mut h, "Comma (1,5)", 0);
        for label in [
            "Always on a box",
            "List the context layers",
            "Colorbar",
            "Scale bar",
            "Veil the map outside the extent if the legend needs room",
        ] {
            click_box(&mut h, label);
        }
        let want = Elements {
            locator: LocatorChoice::Inset,
            north: NorthChoice::None,
            legend: true,
            legend_box: true,
            context: true,
            colorbar: false,
            scale_bar: false,
            veil: false,
            comma: true,
        };
        assert_eq!(draft(&h).elements, want);
        click_box(&mut h, "Legend");
        assert!(!draft(&h).elements.legend);
    }

    /// What the sheet can't use is off, and says why.
    #[test]
    fn what_the_sheet_cant_use_is_off() {
        let h = step_of(|_| {});
        for label in ["Always on a box", "List the context layers", "Scale bar", "Veil the map outside the extent if the legend needs room"]
        {
            assert!(!off(&h, label), "classic: {label}");
        }
        let h = step_of(|app| app.dialogs.publication.as_mut().unwrap().style = "panel".into());
        assert!(off(&h, "Always on a box") && off(&h, "Veil the map outside the extent if the legend needs room"));
        assert!(h.query_by_label_contains("a box of its own").is_some(), "said");
        let h = step_of(|app| app.dialogs.publication.as_mut().unwrap().elements.legend = false);
        assert!(off(&h, "List the context layers") && off(&h, "Always on a box"));
        let h = step_of(globe);
        assert!(off(&h, "Scale bar"));
        assert!(h.query_by_label_contains("whole globe draws no scale bar").is_some(), "said");
    }

    /// On a narrow screen the step's controls stay on it.
    #[test]
    fn the_step_stays_on_a_narrow_screen() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Elements;
        app.dialogs.publication.as_mut().unwrap().style = "panel".into();
        let h = harness_sized(app, egui::vec2(320.0, 900.0));
        for label in ["Default: Globe", "Inset", "Comma (1,5)"] {
            let rect = h.get_by_role_and_label(Role::RadioButton, label).rect();
            assert!(rect.left() >= 0.0 && rect.right() <= 320.0, "{label} at {rect:?}");
        }
        let rect = ticked_box(&h, "Veil the map outside the extent if the legend needs room").rect();
        assert!(rect.right() <= 320.0, "{rect:?}");
    }

    /// The review lists what is on, on a sheet and on a figure.
    #[test]
    fn the_review_lists_the_elements() {
        for (size, want) in [
            ("a4", "Locator: Default (globe)\nNorth arrow: Auto (none from 20°)\nLegend\nScale bar\nDecimal point"),
            ("figure-1col", "North arrow: Auto (none from 20°)\nLegend\nScale bar\nDecimal point"),
        ] {
            let h = step_of(|app| {
                let d = app.dialogs.publication.as_mut().unwrap();
                (d.step, d.size) = (Step::Review, size.into());
            });
            assert!(h.query_by_label(want).is_some(), "{size}");
        }
        let h = step_of(|app| {
            raster(app);
            let d = app.dialogs.publication.as_mut().unwrap();
            d.step = Step::Review;
            d.elements = Elements {
                locator: LocatorChoice::Inset,
                north: NorthChoice::None,
                legend_box: true,
                context: true,
                scale_bar: false,
                veil: false,
                comma: true,
                ..Elements::default()
            };
        });
        let want = "Locator: Inset\nLegend\nAlways on a box\nContext layers listed\nColorbar\nNo veil\nDecimal comma";
        assert!(h.query_by_label(want).is_some());
    }

    /// Create makes the layout the step asked for; Undo takes it away.
    #[test]
    fn create_leaves_out_what_the_step_left_out() {
        let mut app = app();
        open(&mut app);
        let d = app.dialogs.publication.as_mut().unwrap();
        d.step = Step::Review;
        d.elements = Elements { locator: LocatorChoice::None, legend: false, comma: true, ..Elements::default() };
        let before = app.session.project.layouts.len();
        let mut h = harness(app);
        click(&mut h, "Create");
        assert_eq!(h.state().session.project.layouts.len(), before + 1);
        let l = h.state().session.project.layouts.last().unwrap();
        assert!(l.decimal_comma);
        assert!(!l.elements.iter().any(|e| matches!(e.kind, ElementKind::Legend { .. })), "no legend");
        let locators = l.elements.iter().filter(|e| matches!(e.kind, ElementKind::MapFrame { extent_of: Some(_), .. }));
        assert_eq!(locators.count(), 0, "no locator");
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        assert_eq!(h.state().session.project.layouts.len(), before);
    }
}
