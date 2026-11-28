//! The wizard's elements step: what is drawn around the map, and in which mode.
use super::arrange::{LOCATOR, NORTH_ARROW, SCALE_BAR, surround_labels};
use super::*;
use rustgis_engine::cmd::layout::{publication_style_cards, publication_style_has_column};
use rustgis_engine::map::Drawn;
use rustgis_engine::map::layout::{ArrowStyle, MAX_LEGEND_COLUMNS, ScaleBarStyle, ScaleUnits, colorbar_layer};
use rustgis_engine::render::layout::{legend_lists, tile_listed};

/// The longest legend title `layout.publication` takes, in characters.
const MAX_LEGEND_TITLE: usize = 200;

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

/// A layer ticked out of the legend: a map layer by its instance, as it may be renamed while the
/// wizard is open; a web layer, which has none, by its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExcludedLayer {
    Layer(u64),
    Web(String),
}

/// What the step asks. A control the sheet can't use keeps its answer, which isn't sent.
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
    /// Whether the map frame has its grid; the map's own graticule is drawn either way.
    pub frame_grid: bool,
    /// A decimal comma rather than a point.
    pub comma: bool,
    pub north_style: ArrowStyle,
    pub scale_units: ScaleUnits,
    pub scale_style: ScaleBarStyle,
    /// The legend's title; blank, it has none.
    pub legend_title: String,
    /// The legend's columns, or `None` for the placement's pick.
    pub legend_columns: Option<usize>,
    /// The layers ticked out of the legend.
    pub legend_exclude: Vec<ExcludedLayer>,
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
            frame_grid: true,
            comma: false,
            north_style: ArrowStyle::default(),
            scale_units: ScaleUnits::default(),
            scale_style: ScaleBarStyle::default(),
            legend_title: String::new(),
            legend_columns: None,
            legend_exclude: Vec::new(),
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
    NorthStyle,
    /// The scale bar's units and style.
    ScaleLook,
    /// The legend's title and columns.
    LegendLook,
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
            Control::NorthStyle => self.draws_arrow(s),
            Control::ScaleLook => self.draws_scale_bar(s),
            Control::LegendLook => self.elements.legend && listed(s).iter().any(|l| !l.left_out(&self.elements)),
        }
    }

    /// The north arrow's style, when one other than the default is picked for an arrow drawn.
    fn north_look(&self, s: &Session) -> Option<ArrowStyle> {
        let style = self.elements.north_style;
        (style != ArrowStyle::default() && self.usable(Control::NorthStyle, s)).then_some(style)
    }

    /// The scale bar's units and style, each when picked for a bar drawn.
    fn scale_look(&self, s: &Session) -> (Option<ScaleUnits>, Option<ScaleBarStyle>) {
        if !self.usable(Control::ScaleLook, s) {
            return (None, None);
        }
        let (units, style) = (self.elements.scale_units, self.elements.scale_style);
        ((units != ScaleUnits::default()).then_some(units), (style != ScaleBarStyle::default()).then_some(style))
    }

    /// The legend's title and columns, each when given for a legend with rows.
    fn legend_look(&self, s: &Session) -> (Option<&str>, Option<usize>) {
        if !self.usable(Control::LegendLook, s) {
            return (None, None);
        }
        let title = self.elements.legend_title.trim();
        ((!title.is_empty()).then_some(title), self.elements.legend_columns)
    }

    /// The names of the layers ticked out of a legend drawn, as the map now spells them.
    fn legend_exclude(&self, s: &Session) -> Vec<String> {
        if !self.elements.legend {
            return Vec::new();
        }
        listed(s).into_iter().filter(|l| l.left_out(&self.elements)).map(|l| l.name.to_string()).collect()
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
        // A look makes its element an object; without one it goes as its mode alone.
        let north = match (e.north, self.north_look(s)) {
            (NorthChoice::None, _) => Some("none".into()),
            (NorthChoice::Always, None) => Some("always".into()),
            (NorthChoice::Auto, None) => None,
            (NorthChoice::Always, Some(style)) => Some(json!({"show": "always", "style": style.code()})),
            (NorthChoice::Auto, Some(style)) => Some(json!({"show": "auto", "style": style.code()})),
        };
        let (units, style) = self.scale_look(s);
        let scale_bar = if !e.scale_bar && self.usable(Control::ScaleBar, s) {
            Some("none".into())
        } else {
            object([("units", units.map(|u| u.code().into())), ("style", style.map(|v| v.code().into()))])
        };
        let (title, columns) = self.legend_look(s);
        let exclude = self.legend_exclude(s);
        let legend = if !e.legend {
            Some("none".into())
        } else {
            let exclude = (!exclude.is_empty()).then(|| exclude.into());
            object([("title", title.map(Value::from)), ("columns", columns.map(Value::from)), ("exclude", exclude)])
        };
        for (key, value) in [
            ("locator", locator.map(Value::from)),
            ("northArrow", north),
            ("legend", legend),
            ("legendBox", (e.legend_box && self.usable(Control::LegendBox, s)).then(|| "always".into())),
            ("contextInLegend", (e.context && self.usable(Control::Context, s)).then(|| true.into())),
            ("colorbar", (!e.colorbar && self.usable(Control::Colorbar, s)).then(|| "none".into())),
            ("scaleBar", scale_bar),
            ("grid", (!e.frame_grid).then(|| "none".into())),
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

/// The object of the options given, or `None` with none.
fn object<const N: usize>(options: [(&str, Option<Value>); N]) -> Option<Value> {
    let given: serde_json::Map<String, Value> = options.into_iter().filter_map(|(k, v)| Some((k.to_string(), v?))).collect();
    (!given.is_empty()).then(|| given.into())
}

/// A name the legend lists, and every layer under it, any case: the command leaves out by name.
struct Listed<'a> {
    name: &'a str,
    layers: Vec<ExcludedLayer>,
}

impl Listed<'_> {
    fn left_out(&self, e: &Elements) -> bool {
        self.layers.iter().any(|l| e.legend_exclude.contains(l))
    }
}

/// What a legend of the active map can give rows to, top first: the layers it shows and can list,
/// and the web layers it lists by their picture. The sheet's scale and context layers, which can
/// still leave the legend empty, are known only to the command.
fn listed(s: &Session) -> Vec<Listed<'_>> {
    let map = s.map();
    let mut out: Vec<Listed> = Vec::new();
    let mut at = std::collections::HashMap::new();
    for item in map.stack() {
        let found = match item {
            Drawn::Layer(i) => map
                .layers
                .get(i)
                .filter(|l| map.shows(l) && legend_lists(l))
                .map(|l| (l.name.as_str(), ExcludedLayer::Layer(l.instance_id()))),
            Drawn::Tile(i) => map.tiles.get(i).filter(|t| tile_listed(t)).map(|t| (t.name.as_str(), ExcludedLayer::Web(t.name.clone()))),
        };
        let Some((name, layer)) = found else { continue };
        let i = *at.entry(name.to_ascii_lowercase()).or_insert_with(|| {
            out.push(Listed { name, layers: Vec::new() });
            out.len() - 1
        });
        out[i].layers.push(layer);
    }
    out
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

/// The parts there are, on one line.
fn joined<const N: usize>(parts: [Option<&str>; N]) -> String {
    parts.into_iter().flatten().collect::<Vec<_>>().join(" · ")
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
        let style = d.north_look(s).map(|v| tl_raw!(v.label()));
        let choice = joined([Some(when), style]);
        on.push(i18n::fmt("{surround}: {choice}", &[("surround", surround_labels()[NORTH_ARROW]), ("choice", &choice)]));
    }
    // The legend and scale bar by name, and the looks sent with them: the parts in logical order,
    // each line put in display form once, as a whole.
    let looked = |name: &str, look: String| match look.is_empty() {
        true => i18n::display(name),
        false => i18n::fmt("{surround}: {choice}", &[("surround", name), ("choice", &look)]),
    };
    let (title, columns) = d.legend_look(s);
    let title = title.map(|t| i18n::fmt_raw("“{title}”", &[("title", t)]));
    let columns = columns.map(|n| i18n::fmt_raw("Columns: {count}", &[("count", &n.to_string())]));
    let exclude = d.legend_exclude(s);
    let exclude = (!exclude.is_empty()).then(|| i18n::fmt_raw("Left out: {layers}", &[("layers", &exclude.join(", "))]));
    let legend = looked(tl_raw!("Legend"), joined([title.as_deref(), columns.as_deref(), exclude.as_deref()]));
    let (units, style) = d.scale_look(s);
    let scale = joined([units.map(|u| tl_raw!(u.label())), style.map(|v| tl_raw!(v.label()))]);
    let scale = looked(tl_raw!("Scale bar"), scale);
    let mut push_if = |is: bool, line: &str| {
        if is {
            on.push(line.to_string());
        }
    };
    push_if(e.legend, &legend);
    push_if(e.legend_box && d.usable(Control::LegendBox, s), tl!("Always on a box"));
    push_if(e.context && d.usable(Control::Context, s), tl!("Context layers listed"));
    push_if(d.draws_colorbar(s), tl!("Colorbar"));
    push_if(d.draws_scale_bar(s), &scale);
    push_if(!e.frame_grid, tl!("No frame grid"));
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
    let usable = d.usable(Control::NorthStyle, s);
    ui.indent("publication_wizard_north", |ui| {
        let why = tl!("No north arrow is drawn.");
        let style = &mut d.elements.north_style;
        pick_list(ui, "publication_wizard_north_style", tl!("Style"), (usable, why), style, &ArrowStyle::ALL, |v| tl!(v.label()));
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
        legend_look_rows(ui, s, d);
        legend_exclude_rows(ui, s, d);
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
    let usable = d.usable(Control::ScaleLook, s);
    ui.indent("publication_wizard_scale", |ui| {
        let why = tl!("No scale bar is drawn.");
        let e = &mut d.elements;
        pick_list(ui, "publication_wizard_scale_units", tl!("Units"), (usable, why), &mut e.scale_units, &ScaleUnits::ALL, |v| {
            tl!(v.label())
        });
        pick_list(ui, "publication_wizard_scale_style", tl!("Style"), (usable, why), &mut e.scale_style, &ScaleBarStyle::ALL, |v| {
            tl!(v.label())
        });
    });
    ui.checkbox(&mut d.elements.frame_grid, tl!("Frame grid"));
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

/// A caption and the list of `all` that picks `value`, off with its reason.
fn pick_list<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    id: &str,
    caption: &str,
    (usable, why): (bool, &str),
    value: &mut T,
    all: &[T],
    name: impl Fn(T) -> &'static str,
) {
    ui.horizontal_wrapped(|ui| {
        let caption = ui.label(caption);
        ui.add_enabled_ui(usable, |ui| {
            egui::ComboBox::from_id_salt(id)
                .selected_text(name(*value))
                .show_ui(ui, |ui| {
                    for &v in all {
                        ui.selectable_value(value, v, name(v));
                    }
                })
                .response
                .labelled_by(caption.id)
                .on_disabled_hover_text(why);
        });
    });
}

/// The legend's title and columns.
fn legend_look_rows(ui: &mut egui::Ui, s: &Session, d: &mut Draft) {
    let usable = d.usable(Control::LegendLook, s);
    let why = if !d.elements.legend {
        tl!("There is no legend to title or split.")
    } else if listed(s).is_empty() {
        tl!("The map has no layer the legend lists.")
    } else {
        tl!("Every layer is left out of the legend.")
    };
    ui.horizontal_wrapped(|ui| {
        let caption = ui.label(tl!("Title"));
        let edit = egui::TextEdit::singleline(&mut d.elements.legend_title).char_limit(MAX_LEGEND_TITLE).desired_width(field_width(ui));
        ui.add_enabled(usable, edit).labelled_by(caption.id).on_disabled_hover_text(why);
    });
    let columns: Vec<Option<usize>> = std::iter::once(None).chain((1..=MAX_LEGEND_COLUMNS).map(Some)).collect();
    ui.horizontal_wrapped(|ui| {
        let caption = ui.label(tl!("Columns"));
        ui.add_enabled_ui(usable, |ui| {
            let name = |n: Option<usize>| n.map_or_else(|| tl!("Auto").to_string(), |n| n.to_string());
            egui::ComboBox::from_id_salt("publication_wizard_legend_columns")
                .selected_text(name(d.elements.legend_columns))
                .show_ui(ui, |ui| {
                    for n in columns {
                        ui.selectable_value(&mut d.elements.legend_columns, n, name(n));
                    }
                })
                .response
                .labelled_by(caption.id)
                .on_disabled_hover_text(why);
        });
    });
    if d.elements.legend && !usable {
        ui.label(egui::RichText::new(why).small().weak());
    }
}

/// A box per name the legend lists, ticked to leave its layers out.
fn legend_exclude_rows(ui: &mut egui::Ui, s: &Session, d: &mut Draft) {
    let listed = listed(s);
    if listed.is_empty() {
        return;
    }
    ui.label(tl!("Leave out of the legend"));
    for l in listed {
        let mut out = l.left_out(&d.elements);
        let why = tl!("There is no legend to leave it out of.");
        let tick = egui::Checkbox::new(&mut out, l.name);
        if ui.add_enabled(d.elements.legend, tick).on_disabled_hover_text(why).changed() {
            d.elements.legend_exclude.retain(|k| !l.layers.contains(k));
            if out {
                d.elements.legend_exclude.extend(l.layers);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::harness::*;
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT, Queryable};
    use rustgis_engine::data::ogc::Legend;
    use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster};
    use rustgis_engine::map::layout::ElementKind;
    use rustgis_engine::map::{Projection, RasterLayer, TileKind, TileLayer};

    type Wizard = egui_kittest::Harness<'static, RustGisApp>;

    /// The keys the step may send.
    const KEYS: [&str; 10] =
        ["grid", "locator", "northArrow", "legend", "legendBox", "contextInLegend", "colorbar", "scaleBar", "veil", "decimal"];

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
        ticked_box(h, label).scroll_to_me();
        h.run_steps(4);
        ticked_box(h, label).click();
        h.run_steps(4);
    }

    /// The `nth` radio button called `label`, top to bottom: None is both the locator's and the
    /// north arrow's.
    fn pick(h: &mut Wizard, label: &str, nth: usize) {
        // The step scrolls: one below the window is brought into view first.
        h.get_all_by_role_and_label(Role::RadioButton, label).nth(nth).expect("the radio").scroll_to_me();
        h.run_steps(4);
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
        let cases: [(Set, Value); 13] = [
            (|e| e.frame_grid = false, json!({"grid": "none"})),
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

    /// Elements with a look picked for each of the three.
    fn looks() -> Elements {
        Elements {
            north_style: ArrowStyle::Compass,
            scale_units: ScaleUnits::Kilometers,
            scale_style: ScaleBarStyle::Line,
            legend_title: "  Key  ".into(),
            legend_columns: Some(3),
            ..Elements::default()
        }
    }

    /// A look picked sends its element as an object, the title trimmed, on a sheet and a figure.
    #[test]
    fn a_look_picked_is_sent_with_its_element() {
        let app = app();
        for size in ["a4", "figure-1col"] {
            let mut d = Draft::new(&app.session);
            d.size = size.into();
            d.elements = looks();
            let want = json!({
                "northArrow": {"show": "auto", "style": "compass"},
                "scaleBar": {"units": "kilometers", "style": "line"},
                "legend": {"title": "Key", "columns": 3},
            });
            assert_eq!(added(&app.session, &d), want, "{size}");
            d.elements = Elements { north: NorthChoice::Always, north_style: ArrowStyle::Star, ..Elements::default() };
            assert_eq!(added(&app.session, &d), json!({"northArrow": {"show": "always", "style": "star"}}), "{size}");
        }
    }

    /// Only what differs from the default is sent: a default style, a blank title and Auto columns
    /// are left to the command.
    #[test]
    fn only_what_differs_from_the_default_is_sent() {
        let app = app();
        let mut d = Draft::new(&app.session);
        d.elements = Elements { north: NorthChoice::Always, legend_title: "   ".into(), ..Elements::default() };
        assert_eq!(added(&app.session, &d), json!({"northArrow": "always"}));
        d.elements = Elements { scale_style: ScaleBarStyle::Stepped, legend_columns: Some(1), ..Elements::default() };
        assert_eq!(added(&app.session, &d), json!({"scaleBar": {"style": "stepped"}, "legend": {"columns": 1}}));
        d.elements = Elements { scale_units: ScaleUnits::Feet, legend_title: "Key".into(), ..Elements::default() };
        assert_eq!(added(&app.session, &d), json!({"scaleBar": {"units": "feet"}, "legend": {"title": "Key"}}));
    }

    /// The look of an element left out isn't sent: the element goes as before.
    #[test]
    fn the_look_of_an_element_left_out_isnt_sent() {
        let mut app = app();
        let mut d = Draft::new(&app.session);
        d.elements = Elements { north: NorthChoice::None, legend: false, scale_bar: false, ..looks() };
        assert_eq!(added(&app.session, &d), json!({"northArrow": "none", "legend": "none", "scaleBar": "none"}));
        globe(&mut app);
        d.elements = looks();
        assert_eq!(added(&app.session, &d), json!({"legend": {"title": "Key", "columns": 3}}), "a whole globe");
    }

    /// With no layer for the legend to list, its title and columns aren't sent: a raster on a
    /// ramp is keyed by the colorbar, a hidden layer by nothing. A web layer with a legend
    /// picture is one to list.
    #[test]
    fn with_nothing_to_list_the_legend_has_no_look() {
        let mut app = app();
        let mut d = Draft::new(&app.session);
        d.elements = Elements { legend_title: "Key".into(), legend_columns: Some(2), ..Elements::default() };
        for l in &mut app.session.map_mut().layers {
            l.visible = false;
        }
        assert_eq!(added(&app.session, &d), json!({}), "all hidden");
        raster(&mut app);
        assert_eq!(added(&app.session, &d), json!({}), "a raster on a ramp");
        let mut tile = TileLayer::new("Parcels", TileKind::Raster, "https://tiles.invalid/{z}/{x}/{y}.png");
        tile.legends = vec![Legend { url: "https://tiles.invalid/legend.png".into(), ..Default::default() }];
        app.session.map_mut().tiles.push(tile);
        assert_eq!(added(&app.session, &d), json!({"legend": {"title": "Key", "columns": 2}}), "a web layer's picture");
    }

    /// The `nth` combo box captioned `caption`, top to bottom: Style is both the north arrow's
    /// and the scale bar's.
    fn combo<'a>(h: &'a Wizard, caption: &'a str, nth: usize) -> egui_kittest::Node<'a> {
        h.get_all_by_role_and_label(Role::ComboBox, caption).nth(nth).expect("the list")
    }

    fn choose(h: &mut Wizard, caption: &str, nth: usize, value: &str) {
        // The step scrolls: a list below the window is brought into view first.
        combo(h, caption, nth).scroll_to_me();
        h.run_steps(4);
        combo(h, caption, nth).click();
        h.run_steps(4);
        h.get_by_label(value).click();
        h.run_steps(4);
    }

    /// Each list and the title box set their answer.
    #[test]
    fn the_look_controls_set_the_answers() {
        let mut h = step_of(|_| {});
        choose(&mut h, "Style", 0, "Compass rose");
        choose(&mut h, "Units", 0, "Kilometers");
        choose(&mut h, "Style", 1, "Single line");
        choose(&mut h, "Columns", 0, "3");
        h.get_by_role_and_label(Role::TextInput, "Title").focus();
        h.run_steps(4);
        h.get_by_role_and_label(Role::TextInput, "Title").type_text("  Key  ");
        h.run_steps(4);
        assert_eq!(draft(&h).elements, looks());
    }

    /// A look with no element to take it is off; its answer is kept.
    #[test]
    fn a_look_with_no_element_is_off() {
        let off_combo = |h: &Wizard, caption: &str, nth| combo(h, caption, nth).accesskit_node().is_disabled();
        let title_off = |h: &Wizard| h.get_by_role_and_label(Role::TextInput, "Title").accesskit_node().is_disabled();
        let h = step_of(|_| {});
        assert!(!off_combo(&h, "Style", 0) && !off_combo(&h, "Style", 1) && !off_combo(&h, "Units", 0));
        assert!(!off_combo(&h, "Columns", 0) && !title_off(&h));
        let h = step_of(|app| {
            let e = &mut app.dialogs.publication.as_mut().unwrap().elements;
            *e = Elements { north: NorthChoice::None, legend: false, scale_bar: false, ..looks() };
        });
        assert!(off_combo(&h, "Style", 0) && off_combo(&h, "Style", 1) && off_combo(&h, "Units", 0));
        assert!(off_combo(&h, "Columns", 0) && title_off(&h));
        assert_eq!(draft(&h).elements.legend_title, "  Key  ", "kept");
        let h = step_of(|app| {
            for l in &mut app.session.map_mut().layers {
                l.visible = false;
            }
        });
        assert!(off_combo(&h, "Columns", 0) && title_off(&h), "nothing to list");
        assert!(h.query_by_label_contains("no layer the legend lists").is_some(), "said");
    }

    /// The review names the looks that are sent, and no default one.
    #[test]
    fn the_review_names_the_looks() {
        let review = |e: Elements| {
            step_of(|app| {
                let d = app.dialogs.publication.as_mut().unwrap();
                (d.step, d.elements) = (Step::Review, e);
            })
        };
        let h = review(looks());
        let want = "Locator: Default (globe)\nNorth arrow: Auto (none from 20°) · Compass rose\n\
                    Legend: “Key” · Columns: 3\nScale bar: Kilometers · Single line\nDecimal point";
        assert!(h.query_by_label(want).is_some());
        let h = review(Elements { legend: false, ..looks() });
        assert!(h.query_by_label_contains("Key").is_none(), "no legend, no title");
    }

    /// In a right-to-left language the legend's and scale bar's lines are put in display form once,
    /// as a whole, with a look and without one.
    #[test]
    fn the_review_shows_the_looks_right_to_left() {
        let app = app();
        let previous = i18n::current();
        i18n::set_current(i18n::lang_from_tag("fa").unwrap());
        let mut d = Draft::new(&app.session);
        let title = "نقشه GIS 2026";
        d.elements = Elements { legend_title: title.into(), legend_columns: Some(2), ..Elements::default() };
        let looked = elements_value(&app.session, &d);
        let parts =
            format!("{} · {}", i18n::fmt_raw("“{title}”", &[("title", title)]), i18n::fmt_raw("Columns: {count}", &[("count", "2")]));
        let legend = i18n::fmt("{surround}: {choice}", &[("surround", tl_raw!("Legend")), ("choice", &parts)]);
        let bare = [i18n::display(tl_raw!("Legend")), i18n::display(tl_raw!("Scale bar"))];
        d.elements = Elements::default();
        let plain = elements_value(&app.session, &d);
        i18n::set_current(previous);
        assert!(looked.lines().any(|l| l == legend), "{looked}");
        assert!(looked.lines().any(|l| l == bare[1]), "{looked}");
        assert!(plain.lines().any(|l| l == bare[0]), "{plain}");
    }

    /// Create draws the looks picked; Undo takes the layout away.
    #[test]
    fn create_draws_the_looks_picked() {
        let mut app = app();
        open(&mut app);
        let d = app.dialogs.publication.as_mut().unwrap();
        d.step = Step::Review;
        d.elements = Elements { north: NorthChoice::Always, ..looks() };
        let before = app.session.project.layouts.len();
        let mut h = harness(app);
        click(&mut h, "Create");
        let l = h.state().session.project.layouts.last().unwrap();
        let find = |f: &dyn Fn(&ElementKind) -> bool| l.elements.iter().any(|e| f(&e.kind));
        assert!(find(&|k| matches!(k, ElementKind::NorthArrow { style: ArrowStyle::Compass, .. })), "the arrow");
        let bar = |k: &ElementKind| matches!(k, ElementKind::ScaleBar { units: ScaleUnits::Kilometers, style: ScaleBarStyle::Line, .. });
        assert!(find(&bar), "the scale bar");
        assert!(find(&|k| matches!(k, ElementKind::Legend { title, columns: 3, .. } if title == "Key")), "the legend");
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        assert_eq!(h.state().session.project.layouts.len(), before);
    }

    /// A raster on a colour scale on top of the map.
    fn raster(app: &mut RustGisApp) {
        let values: Vec<f32> = (0..16).map(|v| v as f32).collect();
        let at = GeoTransform { origin_x: -20.0, origin_y: 10.0, pixel_width: 10.0, pixel_height: -5.0 };
        let r = Raster::new("dem", 4, 4, vec![values], at, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        app.session.map_mut().add_raster("Heights", RasterLayer::from_path(r, "dem.tif"));
    }

    /// The sample map's layers replaced by copies of its first, one per name, top first.
    fn named(app: &mut RustGisApp, names: &[&str]) {
        let map = app.session.map_mut();
        let first = map.layers[0].clone();
        map.layers.clear();
        for name in names.iter().rev() {
            let mut l = first.clone();
            l.name = name.to_string();
            map.add_layer(l);
        }
    }

    /// A web layer with a legend picture.
    fn web(app: &mut RustGisApp, name: &str) {
        let mut tile = TileLayer::new(name, TileKind::Raster, "https://tiles.invalid/{z}/{x}/{y}.png");
        tile.legends = vec![Legend { url: "https://tiles.invalid/legend.png".into(), ..Default::default() }];
        app.session.map_mut().tiles.push(tile);
    }

    fn out_of(app: &RustGisApp, names: &[&str]) -> Vec<ExcludedLayer> {
        let map = app.session.map();
        let key = |name: &&str| match map.layers.iter().find(|l| l.name == *name) {
            Some(l) => ExcludedLayer::Layer(l.instance_id()),
            None => ExcludedLayer::Web(name.to_string()),
        };
        names.iter().map(key).collect()
    }

    /// The layers ticked are sent by name, with the legend's other answers.
    #[test]
    fn the_layers_ticked_are_left_out() {
        let mut app = app();
        named(&mut app, &["Cities", "Roads"]);
        web(&mut app, "Parcels");
        for size in ["a4", "figure-1col"] {
            let mut d = Draft::new(&app.session);
            d.size = size.into();
            d.elements.legend_exclude = out_of(&app, &["Cities", "Parcels"]);
            assert_eq!(added(&app.session, &d), json!({"legend": {"exclude": ["Cities", "Parcels"]}}), "{size}");
            d.elements.legend_title = "Key".into();
            let want = json!({"legend": {"title": "Key", "exclude": ["Cities", "Parcels"]}});
            assert_eq!(added(&app.session, &d), want, "{size}");
        }
    }

    /// The list is what the legend can list: not a hidden layer, a graphics layer or a raster on
    /// a ramp, ticked or not.
    #[test]
    fn only_what_the_legend_lists_is_offered() {
        let prepare = |app: &mut RustGisApp| {
            named(app, &["Cities", "Hidden", "Drawings"]);
            raster(app);
            for l in &mut app.session.map_mut().layers {
                l.visible = l.name != "Hidden";
                l.graphics = l.name == "Drawings";
            }
            web(app, "Parcels");
            let out = out_of(app, &["Hidden", "Drawings", "Heights"]);
            app.dialogs.publication.as_mut().unwrap().elements.legend_exclude = out;
        };
        let h = step_of(prepare);
        assert_eq!(added(&h.state().session, draft(&h)), json!({}));
        for label in ["Cities", "Parcels"] {
            assert!(h.query_by_role_and_label(Role::CheckBox, label).is_some(), "{label}");
        }
        for label in ["Hidden", "Drawings", "Heights"] {
            assert!(h.query_by_role_and_label(Role::CheckBox, label).is_none(), "{label}");
        }
    }

    /// Names the same but for their case share one box, as the command leaves out by name.
    #[test]
    fn one_box_leaves_out_every_layer_of_a_name() {
        let mut h = step_of(|app| named(app, &["Cities", "cities", "Roads"]));
        assert!(h.query_by_role_and_label(Role::CheckBox, "cities").is_none(), "one box");
        click_box(&mut h, "Cities");
        assert_eq!(added(&h.state().session, draft(&h)), json!({"legend": {"exclude": ["Cities"]}}));
        // The first renamed, the other is still left out, under its own name.
        h.state_mut().session.map_mut().layers[0].name = "Towns".into();
        h.run_steps(4);
        assert_eq!(added(&h.state().session, draft(&h)), json!({"legend": {"exclude": ["Towns", "cities"]}}));
        click_box(&mut h, "Towns");
        click_box(&mut h, "cities");
        assert!(draft(&h).elements.legend_exclude.is_empty(), "unticked");
    }

    /// A layer renamed, removed or on a map left while the wizard is open is followed.
    #[test]
    fn the_layers_left_out_are_followed() {
        let mut app = app();
        named(&mut app, &["Cities", "Roads"]);
        let mut d = Draft::new(&app.session);
        d.elements.legend_exclude = out_of(&app, &["Cities"]);
        app.session.map_mut().layers[0].name = "Towns".into();
        assert_eq!(added(&app.session, &d), json!({"legend": {"exclude": ["Towns"]}}), "renamed");
        let removed = app.session.map_mut().layers.remove(0);
        assert_eq!(added(&app.session, &d), json!({}), "removed");
        app.session.map_mut().layers.insert(0, removed);
        assert_eq!(added(&app.session, &d), json!({"legend": {"exclude": ["Towns"]}}), "back");
        app.run("map.new", json!({"name": "Other"})).unwrap();
        assert_eq!(added(&app.session, &d), json!({}), "another map");
    }

    /// A web layer is ticked by its name: renamed, it is offered unticked, and the tick counts
    /// again under the old name.
    #[test]
    fn a_web_layer_left_out_is_followed_by_its_name() {
        let mut app = app();
        web(&mut app, "Parcels");
        let mut d = Draft::new(&app.session);
        d.elements.legend_exclude = out_of(&app, &["Parcels"]);
        app.session.map_mut().tiles.last_mut().unwrap().name = "Lots".into();
        assert_eq!(added(&app.session, &d), json!({}), "renamed");
        app.session.map_mut().tiles.last_mut().unwrap().name = "Parcels".into();
        assert_eq!(added(&app.session, &d), json!({"legend": {"exclude": ["Parcels"]}}), "its name again");
    }

    /// With no legend the list is off and nothing is sent; the ticks are kept.
    #[test]
    fn without_a_legend_nothing_is_left_out() {
        let h = step_of(|app| {
            named(app, &["Cities"]);
            let out = out_of(app, &["Cities"]);
            let e = &mut app.dialogs.publication.as_mut().unwrap().elements;
            (e.legend, e.legend_exclude) = (false, out);
        });
        assert!(off(&h, "Cities"));
        assert_eq!(added(&h.state().session, draft(&h)), json!({"legend": "none"}));
        assert_eq!(draft(&h).elements.legend_exclude.len(), 1, "kept");
    }

    /// With every layer left out the legend has no title or columns, and says why.
    #[test]
    fn with_every_layer_left_out_the_legend_has_no_look() {
        let h = step_of(|app| {
            named(app, &["Cities"]);
            let out = out_of(app, &["Cities"]);
            let e = &mut app.dialogs.publication.as_mut().unwrap().elements;
            (e.legend_title, e.legend_columns, e.legend_exclude) = ("Key".into(), Some(2), out);
        });
        assert_eq!(added(&h.state().session, draft(&h)), json!({"legend": {"exclude": ["Cities"]}}));
        assert!(combo(&h, "Columns", 0).accesskit_node().is_disabled());
        assert!(h.query_by_label_contains("Every layer is left out").is_some(), "said");
        assert!(!off(&h, "Cities"), "the way back");
    }

    /// With nothing to list there is no box to tick, and the step says so once.
    #[test]
    fn with_nothing_to_list_no_layer_is_offered() {
        let h = step_of(|app| named(app, &[]));
        assert!(h.query_by_label_contains("Leave out of the legend").is_none());
        assert_eq!(h.query_all_by_label_contains("no layer the legend lists").count(), 1);
    }

    /// The review names the layers left out, each once.
    #[test]
    fn the_review_names_the_layers_left_out() {
        let h = step_of(|app| {
            named(app, &["Cities", "cities", "Roads", "Rivers"]);
            let out = out_of(app, &["Cities", "Rivers"]);
            let d = app.dialogs.publication.as_mut().unwrap();
            (d.step, d.elements.legend_exclude) = (Step::Review, out);
        });
        let want = "Locator: Default (globe)\nNorth arrow: Auto (none from 20°)\n\
                    Legend: Left out: Cities, Rivers\nScale bar\nDecimal point";
        assert!(h.query_by_label(want).is_some());
    }

    /// Create makes the legend without the layers left out; Undo takes the layout away.
    #[test]
    fn create_leaves_the_layers_out_of_the_legend() {
        let mut app = app();
        named(&mut app, &["Cities", "Roads"]);
        open(&mut app);
        let out = out_of(&app, &["Cities"]);
        let d = app.dialogs.publication.as_mut().unwrap();
        (d.step, d.elements.legend_exclude) = (Step::Review, out);
        let before = app.session.project.layouts.len();
        let mut h = harness(app);
        click(&mut h, "Create");
        let l = h.state().session.project.layouts.last().unwrap();
        let left_out = |k: &ElementKind| matches!(k, ElementKind::Legend { exclude, .. } if exclude == &["Cities"]);
        assert!(l.elements.iter().any(|e| left_out(&e.kind)));
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        assert_eq!(h.state().session.project.layouts.len(), before);
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
            ..Elements::default()
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
        for (caption, nth) in [("Style", 0), ("Units", 0), ("Style", 1), ("Columns", 0)] {
            let rect = combo(&h, caption, nth).rect();
            assert!(rect.left() >= 0.0 && rect.right() <= 320.0, "{caption} {nth} at {rect:?}");
        }
        let long = "Municipal boundaries of the northern provinces, revised after the 2026 census";
        let mut app = super::super::harness::app();
        named(&mut app, &[long]);
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Elements;
        let h = harness_sized(app, egui::vec2(320.0, 900.0));
        let rect = ticked_box(&h, long).rect();
        assert!(rect.left() >= 0.0 && rect.right() <= 320.0, "a long name at {rect:?}");
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

    /// The frame grid's box is on, and unticked it sends no grid, on a whole globe too.
    #[test]
    fn the_frame_grid_box_sets_the_answer() {
        for prepare in [(|_| {}) as fn(&mut RustGisApp), globe] {
            let mut h = step_of(prepare);
            assert!(!off(&h, "Frame grid"));
            click_box(&mut h, "Frame grid");
            assert!(!draft(&h).elements.frame_grid);
            assert_eq!(draft(&h).sent(&h.state().session)["grid"], "none");
            click_box(&mut h, "Frame grid");
            assert!(draft(&h).sent(&h.state().session).get("grid").is_none());
        }
    }

    /// The review says a frame grid left out, and nothing for the default.
    #[test]
    fn the_review_says_there_is_no_frame_grid() {
        let mut h = step_of(|app| app.dialogs.publication.as_mut().unwrap().step = Step::Review);
        assert!(h.query_by_label_contains("No frame grid").is_none());
        h.state_mut().dialogs.publication.as_mut().unwrap().elements.frame_grid = false;
        h.run_steps(4);
        let want = "Locator: Default (globe)\nNorth arrow: Auto (none from 20°)\nLegend\nScale bar\nNo frame grid\nDecimal point";
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
