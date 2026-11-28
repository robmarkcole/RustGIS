//! Publication Layout Wizard: `layout.publication` asked step by step (size, map, style, elements,
//! placement, texts, sources and reference, accent), then made at once, as the New Layout ›
//! Publication Layout menu makes it (#2850).
mod accent;
mod arrange;
mod elements;
mod map;
mod review;
mod size;
mod sources;
mod style;
mod texts;

use crate::textform::StatLayer;
use crate::{RustGisApp, i18n, theme::Tokens};
use accent::accent_step;
use arrange::{Arrangement, arrange_step};
use elements::{Elements, elements_step};
use map::{Area, area_extent, map_step};
use review::review_step;
use rustgis_engine::Session;
use rustgis_engine::cmd::layout::{
    MAX_PUBLICATION_LINES, MAX_PUBLICATION_TEXT, PUBLICATION_HEADINGS, PUBLICATION_PROFILES, publication_default_reference,
    publication_style_accent,
};
use rustgis_engine::geom::Extent;
use rustgis_engine::map::Map;
use rustgis_engine::map::layout::PAGES;
use serde_json::{Value, json};
use size::size_step;
use sources::{reference_step, sources_step};
use style::style_step;
use texts::texts_step;

/// The wizard's steps, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    Size,
    Map,
    Style,
    /// What is drawn around the map.
    Elements,
    Placement,
    Texts,
    Sources,
    /// A figure's cartographic reference, the one text it draws.
    Reference,
    Accent,
    Review,
}

impl Step {
    /// Where [`Draft`] keeps what each of the two steps with text fields has of its own: the
    /// texts step's at 0, the sources step's at 1. A figure's reference step has none: it reads as
    /// 0, so [`limit_note`] leaves the refusal out there.
    fn slot(self) -> usize {
        usize::from(self == Step::Sources)
    }
}

/// Why `layout.publication` draws no place for a field's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeftOut {
    NoSources,
    NoReference,
    NotEditorial,
}

/// A text field of the wizard that takes a layer statistic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Title,
    Subtitle,
    Note,
    Sources,
    Author,
    Reference,
    Heading(usize),
}

impl Field {
    /// The sheet's step that asks for it.
    fn step(self) -> Step {
        match self {
            Field::Title | Field::Subtitle | Field::Note | Field::Author | Field::Heading(2) => Step::Texts,
            Field::Sources | Field::Reference | Field::Heading(_) => Step::Sources,
        }
    }
}

/// How a sheet is turned: `Auto` leaves it to `layout.publication` (the way the map comes out larger).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Auto,
    Portrait,
    Landscape,
}

/// What the wizard has asked so far: `layout.publication`'s fields, under the command's names.
pub struct Draft {
    pub step: Step,
    /// A page size's id (`a4`, from [`PAGES`]) or a figure's profile id (`figure-1col`).
    pub size: String,
    pub orientation: Orientation,
    pub area: Area,
    /// The layout's name; empty leaves it to the command.
    pub name: String,
    pub style: String,
    pub title: String,
    /// Whether the title was typed; until then it is the active map's name, as the menu's.
    pub title_typed: bool,
    pub subtitle: String,
    pub note: String,
    pub sources: String,
    pub author: String,
    /// No credit line at all (`"author": ""`), rather than the command's default one.
    pub no_author: bool,
    /// The cartographic reference's lines; empty leaves the command's default.
    pub reference: String,
    /// No cartographic reference at all (`"reference": []`).
    pub no_reference: bool,
    /// The headings of the sources, the reference and an editorial sheet's column; one left empty
    /// is left to the command.
    pub headings: [String; 3],
    /// The field the cursor was last in on the texts step and on the sources step, which a layer
    /// statistic added there goes to ([`Step::slot`]).
    targets: [Option<Field>; 2],
    /// Whether the last statistic added on each of those steps didn't fit its field, said until
    /// that step's text changes or one fits there.
    refused: [bool; 2],
    /// The style's colour, or `None` for the style's own.
    pub accent: Option<egui::Color32>,
    /// The colour last picked, which the swatch comes back with when the box is ticked again.
    picked: Option<egui::Color32>,
    /// What is drawn around the map, and how.
    pub elements: Elements,
    /// Whether the list of steps has yet to bring the current one into view.
    list_follows: bool,
    /// The corners the surrounds take.
    pub arrangement: Arrangement,
    /// Whether Create also sets a chosen arrangement as the favourite.
    pub make_favourite: bool,
}

impl Draft {
    /// Where the wizard starts: an A4 sheet turned Auto (as `layout.publication` turns a size
    /// given alone, the way the map comes out larger; the menu lists Portrait and Landscape
    /// instead), the classic style and the map's name as the title, as the menu titles a sheet.
    pub fn new(s: &Session) -> Self {
        Self {
            step: Step::Size,
            size: "a4".into(),
            orientation: Orientation::Auto,
            area: Area::View,
            name: String::new(),
            style: "classic".into(),
            title: s.map().name.clone(),
            title_typed: false,
            subtitle: String::new(),
            note: String::new(),
            sources: String::new(),
            author: String::new(),
            no_author: false,
            reference: String::new(),
            no_reference: false,
            headings: Default::default(),
            targets: [None; 2],
            refused: [false; 2],
            accent: None,
            picked: None,
            elements: Elements::default(),
            list_follows: true,
            arrangement: Arrangement::Best,
            make_favourite: false,
        }
    }

    /// The accent every style would take, or `None` where each keeps its own: none picked, or
    /// one only seeded from the selected style's default (never customised).
    fn chosen_accent(&self, app_accent: egui::Color32) -> Option<egui::Color32> {
        let own = style_accent(&self.style, app_accent);
        self.accent.filter(|&c| self.picked.is_some() || c != own)
    }

    /// Whether the size is a figure, which takes no style or accent and no text but its reference:
    /// the command draws the map edge to edge, the reference under it, and leaves the rest to the
    /// caption.
    fn figure(&self) -> bool {
        PUBLICATION_PROFILES.iter().any(|p| !p.sheet && p.id == self.size)
    }

    /// The steps this draft goes through, in order.
    fn steps(&self) -> &'static [Step] {
        if self.figure() {
            return &[Step::Size, Step::Map, Step::Elements, Step::Placement, Step::Reference, Step::Review];
        }
        &[Step::Size, Step::Map, Step::Style, Step::Elements, Step::Placement, Step::Texts, Step::Sources, Step::Accent, Step::Review]
    }

    /// Goes to `step`, which the list of steps then brings into view.
    fn go(&mut self, step: Step) {
        self.step = step;
        self.list_follows = true;
    }

    /// The favourite Create sets once the layout is made, if any: what the review says and what
    /// Create does.
    fn favourite_to_set(&self, s: &Session) -> Option<Value> {
        self.arrangement.as_favourite(s).filter(|_| self.make_favourite)
    }

    /// The step `by` places from this one, if there is one.
    fn moved(&self, by: isize) -> Option<Step> {
        let steps = self.steps();
        let at = steps.iter().position(|s| *s == self.step)?;
        steps.get(at.checked_add_signed(by)?).copied()
    }

    /// `layout.publication`'s parameters: the fields set; one left empty is left to the command's
    /// default, but for the title, which empty means none.
    pub fn params(&self) -> Value {
        let profile = match (self.figure(), self.orientation) {
            (true, _) | (false, Orientation::Auto) => self.size.clone(),
            (false, Orientation::Portrait) => format!("{}-portrait", self.size),
            (false, Orientation::Landscape) => format!("{}-landscape", self.size),
        };
        let mut p = json!({"profile": profile});
        if let Some(arrangement) = self.arrangement.value() {
            p["arrangement"] = arrangement;
        }
        if !self.name.trim().is_empty() {
            p["name"] = json!(self.name.trim());
        }
        let reference: Vec<&str> = lines(&self.reference).collect();
        if self.no_reference || !reference.is_empty() {
            p["reference"] = json!(if self.no_reference { Vec::new() } else { reference });
        }
        if self.figure() {
            return p;
        }
        p["style"] = json!(self.style);
        p["title"] = json!(self.title.trim());
        for (key, text) in [("subtitle", &self.subtitle), ("note", &self.note), ("author", &self.author)] {
            if !text.trim().is_empty() {
                p[key] = json!(text.trim());
            }
        }
        if self.no_author {
            p["author"] = json!("");
        }
        let sources: Vec<&str> = lines(&self.sources).collect();
        if !sources.is_empty() {
            p["sources"] = json!(sources);
        }
        let headings = self.headings_sent();
        if !headings.is_empty() {
            p["headings"] = json!(headings);
        }
        if let Some(c) = self.accent {
            p["accent"] = json!(hex(c));
        }
        p
    }

    /// `"headings"` up to the last one given, a gap before it filled with the command's own; none
    /// for a section left out (no sources, no reference), and the column's only on an editorial
    /// sheet, the one style that draws it.
    fn headings_sent(&self) -> Vec<&str> {
        let given = |i: usize| self.left_out(Field::Heading(i)).is_none() && !self.headings[i].trim().is_empty();
        let Some(last) = (0..self.headings.len()).rev().find(|&i| given(i)) else { return Vec::new() };
        (0..=last)
            .map(|i| if given(i) { self.headings[i].trim() } else { PUBLICATION_HEADINGS.get(i).copied().unwrap_or_default() })
            .collect()
    }

    fn editorial(&self) -> bool {
        self.style == "editorial"
    }

    /// Why the sheet has no place for `field`'s text, if it hasn't: what is sent, what the review
    /// says isn't used and which boxes are off all follow from this.
    fn left_out(&self, field: Field) -> Option<LeftOut> {
        match field {
            Field::Heading(0) if lines(&self.sources).next().is_none() => Some(LeftOut::NoSources),
            Field::Reference | Field::Heading(1) if self.no_reference => Some(LeftOut::NoReference),
            Field::Heading(2) if !self.editorial() => Some(LeftOut::NotEditorial),
            _ => None,
        }
    }

    /// Whether `field`'s box is switched off or not shown. The sources' heading stays on with no
    /// sources, which may be typed after it.
    fn off(&self, field: Field) -> bool {
        field == Field::Author && self.no_author || matches!(self.left_out(field), Some(LeftOut::NoReference | LeftOut::NotEditorial))
    }

    fn field_mut(&mut self, field: Field) -> &mut String {
        let [sources, reference, column] = &mut self.headings;
        match field {
            Field::Title => &mut self.title,
            Field::Subtitle => &mut self.subtitle,
            Field::Note => &mut self.note,
            Field::Sources => &mut self.sources,
            Field::Author => &mut self.author,
            Field::Reference => &mut self.reference,
            Field::Heading(0) => sources,
            Field::Heading(1) => reference,
            Field::Heading(_) => column,
        }
    }

    /// Notes `field` as its step's last one while it has the focus, typed into or not (but on a
    /// figure's step, which adds no statistic), and ends its step's refusal note when its text
    /// changes.
    fn edited(&mut self, r: &egui::Response, field: Field) {
        if r.has_focus() && self.step != Step::Reference {
            self.targets[field.step().slot()] = Some(field);
        }
        if r.changed() {
            self.refused[field.step().slot()] = false;
        }
    }

    /// Adds a layer statistic to the field last focused on `own`'s step, or to `own` where none was
    /// or that one is off now; one that would take the field past the limit isn't added.
    fn add_statistic(&mut self, token: &str, own: Field) {
        let slot = own.step().slot();
        let field = self.targets[slot].filter(|&f| !self.off(f)).unwrap_or(own);
        let text = self.field_mut(field);
        if text.chars().count() + token.chars().count() > MAX_PUBLICATION_TEXT {
            self.refused[slot] = true;
            return;
        }
        text.push_str(token);
        self.refused[slot] = false;
        self.title_typed |= field == Field::Title;
    }
}

/// A box's lines as the command takes a list: trimmed, the empty ones left out.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|l| !l.is_empty())
}

/// The menu that adds a layer figure of the active map (the one the sheet shows), listed as the
/// Format pane's Dynamic Text menu lists them; none where the map has no layer to read.
fn statistic_menu(ui: &mut egui::Ui, d: &mut Draft, map: &Map, own: Field) {
    if !StatLayer::any(map) {
        return;
    }
    ui.menu_button(tl!("Insert Statistic"), |ui| {
        crate::textform::stat_layers(ui, &StatLayer::of_map(map), &mut |token| d.add_statistic(&token, own));
    });
}

/// The boxes take no more, cut a longer paste to fit and refuse a statistic past it: said, not
/// done unseen.
fn limit_note(ui: &mut egui::Ui, d: &mut Draft, fields: &[Field]) {
    let limit = [("limit", &*MAX_PUBLICATION_TEXT.to_string())];
    // A figure's step adds no statistic, so no step's refusal is its own.
    let note = if d.step != Step::Reference && d.refused[d.step.slot()] {
        i18n::fmt("The statistic wasn't added: each field takes at most {limit} characters.", &limit)
    } else if fields.iter().any(|&f| !d.off(f) && d.field_mut(f).chars().count() >= MAX_PUBLICATION_TEXT) {
        i18n::fmt("Each field takes at most {limit} characters.", &limit)
    } else {
        return;
    };
    ui.label(egui::RichText::new(note).small().weak());
}

/// A colour as `layout.publication` takes it: `#RRGGBB`.
fn hex(c: egui::Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

/// A page size's or a figure's name, as New Layout › Publication Layout lists it.
fn size_label(id: &str) -> String {
    if let Some((_, label, w, h)) = PAGES.iter().find(|p| p.0 == id) {
        return crate::ribbon::page_label(label, *w, *h);
    }
    PUBLICATION_PROFILES.iter().find(|p| p.id == id).map_or_else(|| id.to_string(), |p| crate::ribbon::profile_label(p).to_string())
}

/// A step's name, as its heading gives it: in logical order (`tl_raw!`), as `i18n::fmt` takes
/// its arguments, converting the whole heading for display once.
fn step_name(step: Step) -> &'static str {
    match step {
        Step::Size => tl_raw!("Size"),
        Step::Map => tl_raw!("Map"),
        Step::Style => tl_raw!("Style"),
        Step::Elements => tl_raw!("Elements"),
        Step::Placement => tl_raw!("Placement"),
        Step::Texts => tl_raw!("Texts"),
        Step::Sources => tl_raw!("Sources and reference"),
        Step::Reference => tl_raw!("Reference"),
        Step::Accent => tl_raw!("Accent"),
        Step::Review => tl_raw!("Review"),
    }
}

/// A step's name as the list shows it, in display form (`tl!`).
fn step_label(step: Step) -> &'static str {
    match step {
        Step::Size => tl!("Size"),
        Step::Map => tl!("Map"),
        Step::Style => tl!("Style"),
        Step::Elements => tl!("Elements"),
        Step::Placement => tl!("Placement"),
        Step::Texts => tl!("Texts"),
        Step::Sources => tl!("Sources"),
        Step::Reference => tl!("Reference"),
        Step::Accent => tl!("Accent"),
        Step::Review => tl!("Review"),
    }
}

/// What a figure leaves out, said where its size is picked and in the review: the texts and
/// accent typed for a sheet aren't drawn on it.
fn figure_note(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new(tl!("A figure is the map edge to edge: its title, texts and style go in its caption.")).small().weak());
}

/// A style's default accent as an egui colour, falling back to `fallback` if the style is unknown.
fn style_accent(style: &str, fallback: egui::Color32) -> egui::Color32 {
    publication_style_accent(style).map_or(fallback, |c| egui::Color32::from_rgb(c.r(), c.g(), c.b()))
}

/// How the wizard names the credit line `layout.publication` writes when none is given: kept as a
/// literal here so catalog extraction sees it, and verified against the engine's
/// `PUBLICATION_DEFAULT_CREDIT` in tests.
fn default_credit() -> String {
    i18n::fmt("Made with RustGIS · {date}", &[("date", tl_raw!("today's date"))])
}

/// A field's width beside its label: 260 points, or what the row leaves once a longer label
/// (Greek "Orientation", in a wider interface font) takes its share, so the row never widens the
/// window.
fn field_width(ui: &egui::Ui) -> f32 {
    ui.available_width().min(260.0)
}

/// The size the window opens at, tall enough for the tallest step in English (the style step's
/// seven styles, their outlines and descriptions), with room for the rounding a high-density
/// screen adds to each line. A narrower screen narrows it (`dialog_width`); on a shorter one
/// egui's window limits its height to the screen.
const SIZE: egui::Vec2 = egui::vec2(600.0, 515.0);

/// The smallest size it can be dragged to.
const MIN_SIZE: egui::Vec2 = egui::vec2(420.0, 360.0);

/// The narrowest screen that has the list of steps beside the fields.
const LIST_FROM: f32 = 480.0;

/// The width of the list of steps.
const LIST_WIDTH: f32 = 110.0;

/// The steps of this draft, each clickable to go to it, in `height`: a shorter window scrolls
/// them, bringing the current one into view once when it changes, not each frame, so the list can
/// be scrolled by hand.
fn step_list(ui: &mut egui::Ui, d: &mut Draft, height: f32) {
    ui.vertical(|ui| {
        ui.set_width(LIST_WIDTH);
        egui::ScrollArea::vertical().id_salt("publication_wizard_steps").max_height(height).auto_shrink([false, false]).show(ui, |ui| {
            let follow = std::mem::take(&mut d.list_follows);
            for &step in d.steps() {
                let width = ui.available_width();
                let item = egui::Button::selectable(d.step == step, step_label(step)).wrap().min_size(egui::vec2(width, 0.0));
                let item = ui.add(item);
                if follow && d.step == step {
                    item.scroll_to_me(None);
                }
                if item.clicked() {
                    d.go(step);
                }
            }
        });
    });
}

/// Shows the wizard while it is open; Create runs `layout.publication` as the menu does, and the
/// wizard stays open on a layout the command refuses.
pub fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(mut d) = app.dialogs.publication.take() else {
        app.dialogs.open = None;
        return;
    };
    // A map opened meanwhile titles its sheet, as the menu titles it at the click.
    if !d.title_typed {
        d.title.clone_from(&app.session.map().name);
    }
    let t = Tokens::get(ctx);
    let (mut open, mut close, mut create) = (true, false, false);
    // Narrower on a screen too narrow for it, as the other dialogs are.
    let frame = egui::Frame::window(&ctx.global_style());
    let screen = ctx.content_rect();
    let width = crate::widgets::dialog_width(ctx, SIZE.x, frame);
    let list = screen.width() >= LIST_FROM;
    egui::Window::new(tl!("Publication Layout Wizard"))
        .id(egui::Id::unique("publication_wizard"))
        .frame(frame)
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(width, SIZE.y))
        .min_width(MIN_SIZE.x.min(width))
        .min_height(MIN_SIZE.y)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(screen.center())
        .show(ctx, |ui| {
            let steps = d.steps();
            let at = steps.iter().position(|s| *s == d.step).unwrap_or(0);
            let heading = i18n::fmt(
                "Step {n} of {count}: {step}",
                &[("n", &(at + 1).to_string()), ("count", &steps.len().to_string()), ("step", step_name(d.step))],
            );
            ui.label(egui::RichText::new(heading).strong());
            ui.separator();
            // The step takes the room the window leaves above the buttons, whatever its own
            // height, so Next stays under the pointer from step to step; a step taller than that
            // scrolls inside it, its text wrapped to its width rather than widening the window.
            // Each step scrolls on its own, so one scrolled down doesn't open the next partway
            // down.
            let size = egui::vec2(84.0, 28.0);
            let spacing = ui.spacing().item_spacing.y;
            // The buttons, and the separator above them (6 points, egui's default) with the
            // spacing on both sides of it.
            let footer = size.y + 6.0 + 2.0 * spacing;
            let body = egui::vec2(ui.available_width(), (ui.available_height() - footer).max(0.0));
            ui.allocate_ui_with_layout(body, egui::Layout::left_to_right(egui::Align::Min), |ui| {
                ui.set_min_size(body);
                if list {
                    step_list(ui, &mut d, body.y);
                    ui.separator();
                }
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical()
                        .id_salt(("publication_wizard_step", d.step))
                        .max_height(body.y)
                        .auto_shrink([false, false])
                        .show(ui, |ui| match d.step {
                            Step::Size => size_step(ui, &mut d),
                            Step::Map => map_step(ui, &app.session, &mut d),
                            Step::Style => style_step(ui, &mut d, t.accent),
                            Step::Elements => elements_step(ui, &app.session, &mut d),
                            Step::Placement => arrange_step(ui, &app.session, &mut d),
                            Step::Texts => texts_step(ui, &mut d, app.session.map()),
                            Step::Sources => sources_step(ui, &mut d, app.session.map()),
                            Step::Reference => reference_step(ui, &mut d),
                            Step::Accent => accent_step(ui, &mut d, t.accent),
                            Step::Review => review_step(ui, &app.session, &d),
                        });
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    match d.moved(1) {
                        Some(next) => {
                            if crate::widgets::dialog_button(ui, tl!("Next"), Some(t.accent), size).clicked() {
                                d.go(next);
                            }
                        }
                        None => create = crate::widgets::dialog_button(ui, tl!("Create"), Some(t.accent), size).clicked(),
                    }
                    let back = d.moved(-1);
                    if ui.add_enabled_ui(back.is_some(), |ui| crate::widgets::dialog_button(ui, tl!("Back"), None, size).clicked()).inner
                        && let Some(back) = back
                    {
                        d.go(back);
                    }
                    close = crate::widgets::dialog_button(ui, tl!("Cancel"), None, size).clicked();
                });
            });
        });
    // A layout the command refuses is said (a map with no extent), and the wizard stays.
    if create {
        let held = d.arrangement.held(&app.session).map_err(String::from);
        match held.and_then(|_| area_extent(&app.session, &d.area)) {
            Ok(extent) => {
                let mut params = d.sent(&app.session);
                if let Some(extent) = extent {
                    params["extent"] = json!(extent);
                }
                if crate::ribbon::make_publication(app, params) {
                    close = true;
                    // Only once the layout is made: a refused one changes no preference.
                    if let Some(favourite) = d.favourite_to_set(&app.session)
                        && let Err(e) = app.run("layout.arrangement", json!({"favourite": favourite}))
                    {
                        app.session.notify(e, true);
                    }
                }
            }
            Err(e) => app.session.notify(e, true),
        }
    }
    if !open || close {
        app.dialogs.open = None;
    } else {
        app.dialogs.publication = Some(d);
    }
}

/// What the tests of the module's files share.
#[cfg(test)]
mod harness {
    use super::*;
    use egui::accesskit::Role;
    use egui::vec2;
    use egui_kittest::kittest::Queryable;

    pub(super) fn app() -> RustGisApp {
        RustGisApp::with_sample()
    }

    pub(super) fn open(app: &mut RustGisApp) {
        app.run("ui.dialog", json!({"name": "publicationWizard"})).unwrap();
    }

    pub(super) fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        harness_sized(app, vec2(1400.0, 1000.0))
    }

    pub(super) fn harness_sized(app: RustGisApp, size: egui::Vec2) -> egui_kittest::Harness<'static, RustGisApp> {
        // The dialogs alone: the ribbon has a Next of its own (the next extent).
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(size).with_pixels_per_point(2.0).with_max_steps(64).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    // The app's fonts and theme at a high-density screen's scale, so sizes, spacing
                    // and the rounding of each line are the ones the app draws there.
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    let theme = app.current_theme();
                    crate::theme::apply(ui.ctx(), &theme.tokens, &theme.metrics);
                    fonts = true;
                    return;
                }
                crate::dialogs::show(app, &ui.ctx().clone());
            },
            app,
        );
        h.run_steps(4);
        h
    }

    pub(super) fn click(h: &mut egui_kittest::Harness<'static, RustGisApp>, label: &str) {
        h.get_by_role_and_label(Role::Button, label).click();
        h.run_steps(4);
    }

    pub(super) fn step(h: &egui_kittest::Harness<'static, RustGisApp>) -> Step {
        h.state().dialogs.publication.as_ref().expect("the wizard is open").step
    }
}

#[cfg(test)]
mod tests {
    use super::harness::*;
    use super::*;
    use egui::accesskit::Role;
    use egui::vec2;
    use egui_kittest::kittest::{NodeT, Queryable};

    /// It opens on its first step: an A4 sheet turned Auto, the classic style, the map's name as
    /// the title and nothing else set; opened again while open, it keeps what was entered.
    #[test]
    fn opens_on_its_defaults_and_keeps_its_draft() {
        let mut app = app();
        open(&mut app);
        let name = app.session.map().name.clone();
        let d = app.dialogs.publication.as_mut().expect("opened");
        assert_eq!((d.step, d.size.as_str(), d.orientation, d.style.as_str()), (Step::Size, "a4", Orientation::Auto, "classic"));
        assert_eq!(d.title, name);
        assert!(d.subtitle.is_empty() && d.note.is_empty() && d.sources.is_empty() && d.author.is_empty());
        assert!(d.accent.is_none() && !d.no_author);
        assert!(d.area == Area::View && d.name.is_empty());
        assert_eq!(app.dialogs.open, Some("publicationWizard"));
        app.dialogs.publication.as_mut().unwrap().subtitle = "Kept".into();
        open(&mut app);
        assert_eq!(app.dialogs.publication.as_ref().unwrap().subtitle, "Kept", "not started over");
    }

    /// What Create runs: the fields set, under the command's names; what is left empty is left to
    /// the command's default, but for the title, which empty means none.
    #[test]
    fn create_sends_the_fields_set_and_nothing_else() {
        let app = app();
        let mut d = Draft::new(&app.session);
        d.title = "Rivers".into();
        assert_eq!(d.params(), json!({"profile": "a4", "style": "classic", "title": "Rivers"}));
        d.orientation = Orientation::Landscape;
        d.size = "letter".into();
        d.style = "panel".into();
        d.title.clear();
        d.subtitle = "2026".into();
        d.note = "A note".into();
        d.sources = "Natural Earth".into();
        d.author = "J. G.".into();
        d.accent = Some(egui::Color32::from_rgb(0x0B, 0x1B, 0x2B));
        assert_eq!(
            d.params(),
            json!({"profile": "letter-landscape", "style": "panel", "title": "", "subtitle": "2026", "note": "A note",
                "sources": ["Natural Earth"], "author": "J. G.", "accent": "#0B1B2B"})
        );
        d.orientation = Orientation::Portrait;
        d.no_author = true;
        assert_eq!(d.params()["profile"], "letter-portrait");
        assert_eq!(d.params()["author"], "", "no credit line");
        // A figure takes no style, texts or accent: the engine draws none of them on it.
        d.size = "figure-1col".into();
        assert_eq!(d.params(), json!({"profile": "figure-1col"}));
    }

    /// A sheet goes through the eight steps; a figure goes from its size to its map, its placement,
    /// its reference and the review, the steps it takes nothing from skipped both ways. Back is off
    /// on the first step; Next becomes Create on the last.
    #[test]
    fn a_sheet_takes_every_step_and_a_figure_skips_what_it_doesnt_draw() {
        let mut app = app();
        open(&mut app);
        let mut h = harness(app);
        assert!(h.get_by_role_and_label(Role::Button, "Back").accesskit_node().is_disabled(), "Back is off");
        for want in [Step::Map, Step::Style, Step::Elements, Step::Placement, Step::Texts, Step::Sources, Step::Accent, Step::Review] {
            click(&mut h, "Next");
            assert_eq!(step(&h), want);
        }
        assert!(h.query_by_role_and_label(Role::Button, "Next").is_none());
        assert!(h.query_by_role_and_label(Role::Button, "Create").is_some());
        for want in [Step::Accent, Step::Sources, Step::Texts, Step::Placement, Step::Elements, Step::Style, Step::Map, Step::Size] {
            click(&mut h, "Back");
            assert_eq!(step(&h), want);
        }
        h.state_mut().dialogs.publication.as_mut().unwrap().size = "figure-2col".into();
        h.run_steps(2);
        for want in [Step::Map, Step::Elements, Step::Placement, Step::Reference, Step::Review] {
            click(&mut h, "Next");
            assert_eq!(step(&h), want);
        }
        for want in [Step::Reference, Step::Placement, Step::Elements, Step::Map, Step::Size] {
            click(&mut h, "Back");
            assert_eq!(step(&h), want);
        }
    }

    /// Create makes the layout in one step, closes the wizard and opens the Publish tab, as the
    /// menu does; Undo takes the layout away.
    #[test]
    fn create_makes_the_layout_and_undo_takes_it_away() {
        let mut app = app();
        open(&mut app);
        {
            let d = app.dialogs.publication.as_mut().unwrap();
            d.style = "banner".into();
            d.subtitle = "From the wizard".into();
            d.step = Step::Review;
        }
        let before = app.session.project.layouts.len();
        let mut h = harness(app);
        click(&mut h, "Create");
        let app = h.state();
        assert_eq!(app.session.project.layouts.len(), before + 1);
        let l = app.session.project.layouts.last().unwrap();
        assert!(
            l.elements.iter().any(
                |e| matches!(&e.kind, rustgis_engine::map::layout::ElementKind::Text { text, .. } if text.contains("From the wizard"))
            ),
            "the subtitle given"
        );
        assert_eq!((app.dialogs.open, app.ui.tab.as_str()), (None, "Publish"));
        assert!(app.dialogs.publication.is_none());
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        assert_eq!(h.state().session.project.layouts.len(), before);
    }

    /// A layout the command refuses (here a style it doesn't know) is said, and the wizard stays
    /// open on its review with nothing made.
    #[test]
    fn a_refused_layout_is_said_and_the_wizard_stays_open() {
        let mut app = app();
        open(&mut app);
        {
            let d = app.dialogs.publication.as_mut().unwrap();
            d.style = "unknown".into();
            d.step = Step::Review;
        }
        let before = app.session.project.layouts.len();
        let mut h = harness(app);
        let said = h.state().session.notices.len();
        click(&mut h, "Create");
        let app = h.state();
        assert_eq!(app.session.project.layouts.len(), before, "nothing made");
        assert!(app.session.notices[said..].iter().any(|n| n.text.contains("unknown")), "said");
        assert_eq!(app.dialogs.open, Some("publicationWizard"));
        assert_eq!(app.dialogs.publication.as_ref().map(|d| d.step), Some(Step::Review));
    }

    /// The names the step list holds, in order, and the one it marks as the current step.
    fn listed(h: &egui_kittest::Harness<'static, RustGisApp>) -> (Vec<&'static str>, Vec<&'static str>) {
        let (mut names, mut current) = (Vec::new(), Vec::new());
        for name in ["Size", "Map", "Style", "Elements", "Placement", "Texts", "Sources", "Reference", "Accent", "Review"] {
            if let Some(n) = h.query_by_role_and_label(Role::Button, name) {
                names.push(name);
                if n.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True) {
                    current.push(name);
                }
            }
        }
        (names, current)
    }

    fn window(h: &egui_kittest::Harness<'static, RustGisApp>) -> egui::Rect {
        h.get_by_role_and_label(Role::Window, "Publication Layout Wizard").rect()
    }

    /// Drags the window's bottom right corner `by` points.
    fn resize(h: &mut egui_kittest::Harness<'static, RustGisApp>, by: egui::Vec2) {
        let corner = window(h).right_bottom() - vec2(1.0, 1.0);
        h.hover_at(corner);
        h.run_steps(2);
        h.drag_at(corner);
        h.run_steps(2);
        h.hover_at(corner + by);
        h.run_steps(2);
        h.drop_at(corner + by);
        h.run_steps(4);
    }

    /// The steps are listed in order with the current one marked; a figure lists only its own,
    /// from the moment its size is picked.
    #[test]
    fn the_steps_are_listed_with_the_current_one_marked() {
        let mut app = app();
        open(&mut app);
        let mut h = harness(app);
        assert_eq!(
            listed(&h),
            (vec!["Size", "Map", "Style", "Elements", "Placement", "Texts", "Sources", "Accent", "Review"], vec!["Size"])
        );
        click(&mut h, "Next");
        assert_eq!(listed(&h).1, ["Map"]);
        click(&mut h, "Back");
        h.state_mut().dialogs.publication.as_mut().unwrap().size = "figure-1col".into();
        h.run_steps(2);
        assert_eq!(listed(&h), (vec!["Size", "Map", "Elements", "Placement", "Reference", "Review"], vec!["Size"]));
    }

    /// A step clicked in the list is the one shown, first, middle or last, keeping what was
    /// entered; Back is off on the first and Next is Create on the last.
    #[test]
    fn a_step_clicked_in_the_list_is_shown() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().subtitle = "Kept".into();
        let mut h = harness(app);
        for (name, want) in [("Review", Step::Review), ("Texts", Step::Texts), ("Size", Step::Size)] {
            click(&mut h, name);
            assert_eq!(step(&h), want);
            assert_eq!(h.query_by_role_and_label(Role::Button, "Create").is_some(), want == Step::Review);
            let back = h.get_by_role_and_label(Role::Button, "Back").accesskit_node().is_disabled();
            assert_eq!(back, want == Step::Size, "{want:?}");
        }
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().subtitle, "Kept");
    }

    /// The smallest window scrolls its list: a list scrolled by hand stays where it was put, and
    /// going to a step brings it into view.
    #[test]
    fn a_short_window_scrolls_its_list_to_the_current_step() {
        let mut app = app();
        open(&mut app);
        let mut h = harness(app);
        resize(&mut h, vec2(-500.0, -400.0));
        // Taller entries, so the nine overflow by far more than a platform's font metrics differ.
        h.ctx.all_styles_mut(|s| s.spacing.interact_size.y = 40.0);
        h.run_steps(30);
        // The top of the list, where its first step is before any scrolling.
        let first = h.get_by_role_and_label(Role::Button, "Size").rect();
        let inside = |h: &egui_kittest::Harness<'static, RustGisApp>, name: &str| {
            let item = h.get_by_role_and_label(Role::Button, name).rect();
            let next = h.get_by_role_and_label(Role::Button, "Next").rect();
            // Above the separator over the buttons (6 points) and the spacing beside it.
            item.top() >= first.top() - 0.5 && item.bottom() <= next.top() - 10.0
        };
        assert!(inside(&h, "Size") && !inside(&h, "Review"), "9 steps don't fit");
        h.hover_at(first.center());
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            source: egui::MouseWheelSource::Wheel,
            delta: vec2(0.0, -500.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        });
        h.run_steps(30);
        assert!(inside(&h, "Review"), "scrolled by hand");
        h.run_steps(10);
        assert!(inside(&h, "Review") && !inside(&h, "Size"), "not pinned to the current step");
        click(&mut h, "Next");
        h.run_steps(30);
        assert!(inside(&h, "Map"), "Next brings its step into view");
    }

    /// Each step in the list is reached with Tab.
    #[test]
    fn tab_reaches_every_step_in_the_list() {
        let mut app = app();
        open(&mut app);
        let mut h = harness(app);
        let mut reached = Vec::new();
        for _ in 0..40 {
            h.key_press(egui::Key::Tab);
            h.run_steps(2);
            for name in ["Size", "Map", "Style", "Elements", "Placement", "Texts", "Sources", "Accent", "Review"] {
                if h.query_by_role_and_label(Role::Button, name).is_some_and(|n| n.accesskit_node().is_focused()) {
                    reached.push(name);
                }
            }
        }
        for name in ["Size", "Map", "Style", "Elements", "Placement", "Texts", "Sources", "Accent", "Review"] {
            assert!(reached.contains(&name), "{name} not reached: {reached:?}");
        }
    }

    /// It opens about 600 by 515 points, and can't be dragged smaller than about 420 by 360;
    /// Next stays inside it.
    #[test]
    fn the_window_opens_at_its_size_and_keeps_a_smallest_one() {
        let mut app = app();
        open(&mut app);
        // Longer than the row: typed texts must not widen the window either.
        app.dialogs.publication.as_mut().unwrap().sources = "Natural Earth and the national mapping agency ".repeat(4);
        let mut h = harness(app);
        let first = window(&h).size();
        assert!((590.0..=640.0).contains(&first.x) && (500.0..=555.0).contains(&first.y), "opens at {first:?}");
        resize(&mut h, vec2(-500.0, -400.0));
        let small = window(&h);
        assert!((415.0..=460.0).contains(&small.width()) && (355.0..=420.0).contains(&small.height()), "smallest {small:?}");
        let next = h.get_by_role_and_label(Role::Button, "Next").rect();
        assert!(small.contains_rect(next), "Next at {next:?} outside {small:?}");
        assert_eq!(listed(&h).0.len(), 9, "the list stays");
        // Each step wraps or scrolls inside that size: none makes the window wider or taller.
        for _ in 0..8 {
            click(&mut h, "Next");
            // Within the point a high-density screen's rounding of a wrapped line may add.
            let now = window(&h).size();
            assert!((now - small.size()).abs().max_elem() <= 1.0, "{:?}: {now:?}", step(&h));
        }
    }

    /// Made larger, the step's area takes the room: Next goes down with the window and stays
    /// there from step to step.
    #[test]
    fn a_larger_window_gives_the_step_the_room() {
        let mut app = app();
        open(&mut app);
        let mut h = harness(app);
        let at = |h: &egui_kittest::Harness<'static, RustGisApp>| h.get_by_role_and_label(Role::Button, "Next").rect();
        let (before, size) = (at(&h), window(&h).size());
        resize(&mut h, vec2(150.0, 150.0));
        let grown = window(&h).size() - size;
        assert!(grown.x > 100.0 && grown.y > 100.0, "grew by {grown:?}");
        let larger = at(&h);
        let moved = larger.right_bottom() - before.right_bottom();
        assert!(moved.y > 50.0, "Next moved by {moved:?}");
        for _ in 0..7 {
            click(&mut h, "Next");
            assert_eq!(at(&h), larger, "{:?}", step(&h));
        }
    }

    /// Next stays where it was from one step to the next: every step is as tall as the tallest.
    #[test]
    fn next_stays_under_the_pointer_from_step_to_step() {
        let mut app = app();
        open(&mut app);
        let mut h = harness(app);
        let at = |h: &egui_kittest::Harness<'static, RustGisApp>| h.get_by_role_and_label(Role::Button, "Next").rect();
        let first = at(&h);
        for _ in 0..7 {
            click(&mut h, "Next");
            assert_eq!(at(&h), first, "{:?}", step(&h));
        }
    }

    /// Next stays where it was in a language whose descriptions run longer: a step taller than
    /// the others scrolls inside its height rather than growing the window.
    #[test]
    fn next_stays_in_place_in_a_longer_language() {
        let previous = crate::i18n::current();
        for code in ["de", "el"] {
            let mut app = app();
            // As the app applies its language each frame (`chosen_lang`), which the harness skips.
            crate::i18n::set_current(crate::i18n::Lang::from_code(code).unwrap());
            open(&mut app);
            let mut h = harness(app);
            let next = crate::i18n::t("Next").to_string();
            assert_ne!(next, "Next", "{code}: the language changed");
            let at = |h: &egui_kittest::Harness<'static, RustGisApp>| h.get_by_role_and_label(Role::Button, &next).rect();
            let first = at(&h);
            for _ in 0..7 {
                h.get_by_role_and_label(Role::Button, &next).click();
                h.run_steps(4);
                assert_eq!(at(&h), first, "{code}: {:?}", step(&h));
            }
        }
        crate::i18n::set_current(previous);
    }

    /// The review's default credit line matches the engine's `PUBLICATION_DEFAULT_CREDIT`.
    #[test]
    fn default_credit_matches_the_engines_constant() {
        use rustgis_engine::cmd::layout::PUBLICATION_DEFAULT_CREDIT;
        assert_eq!(PUBLICATION_DEFAULT_CREDIT, "Made with RustGIS · {date}");
        assert_eq!(default_credit(), PUBLICATION_DEFAULT_CREDIT.replace("{date}", "today's date"));
    }

    /// On a screen narrower than the wizard, it narrows to fit, as the other dialogs do: Next and
    /// the step's text stay on the screen.
    #[test]
    fn a_narrow_screen_keeps_the_wizard_on_it() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Style;
        let h = harness_sized(app, vec2(320.0, 900.0));
        let next = h.get_by_role_and_label(Role::Button, "Next").rect();
        assert!(next.left() >= 0.0 && next.right() <= 320.0, "Next at {next:?} on a screen 320 wide");
        let (_, about) = crate::ribbon::publication_style("panel").unwrap();
        let text = h.get_by_label(about).rect();
        assert!(text.right() <= 320.0, "{text:?} runs off a screen 320 wide");
        assert_eq!(listed(&h).0, Vec::<&str>::new(), "the list is hidden");
        assert!(h.query_by_label("Step 3 of 9: Style").is_some(), "the heading still says the step");
    }

    /// A screen that narrows while the wizard is open (a phone turned, a browser resized) keeps
    /// it on the screen.
    #[test]
    fn a_screen_narrowed_while_open_keeps_the_wizard_on_it() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Style;
        let mut h = harness(app);
        h.set_size(vec2(320.0, 900.0));
        h.run_steps(6);
        let next = h.get_by_role_and_label(Role::Button, "Next").rect();
        assert!(next.left() >= 0.0 && next.right() <= 320.0, "Next at {next:?} on a screen 320 wide");
        let (_, about) = crate::ribbon::publication_style("panel").unwrap();
        let text = h.get_by_label(about).rect();
        assert!(text.right() <= 320.0, "{text:?} runs off a screen 320 wide");
    }

    /// Cancel closes the wizard with nothing made, and the next opening starts over.
    #[test]
    fn cancel_makes_nothing_and_the_next_opening_starts_over() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().subtitle = "Dropped".into();
        let before = app.session.project.layouts.len();
        let mut h = harness(app);
        click(&mut h, "Cancel");
        assert_eq!(h.state().dialogs.open, None);
        assert_eq!(h.state().session.project.layouts.len(), before);
        h.state_mut().run("ui.dialog", json!({"name": "publicationWizard"})).unwrap();
        assert!(h.state().dialogs.publication.as_ref().unwrap().subtitle.is_empty());
    }
}
