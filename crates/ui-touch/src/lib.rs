//! The touch interface for phones (#1902, phase 2): the map with a bar of large buttons under it
//! and sheets that rise over it, in place of the desktop's ribbon and panes.
//!
//! It is drawn around the egui front end's own map view and runs the same commands: a shell
//! turns it on with `app.chrome = Some(rustgis_ui_touch::chrome)`. Dialogs, notices and the
//! Project page stay the front end's.
//!
//! What is here: the map, the bar (Layers, Add Data, Capture, More), the Layers sheet (show,
//! hide, zoom to, the active layer), the Capture sheet (a point, line or polygon placed by the cross
//! on the map, with a form for its layer's attributes that follows the layer's data design: value
//! lists, ranges and feature templates) and the More sheet (full extent, project, undo, the
//! desktop interface, the device's position). Where a shell reports the device's position
//! (`RustGisApp::position`), it is drawn on the map and the capture cross can be put on it.
//! Not yet: editing a shape's vertices, snapping, and text input that suits a soft keyboard.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use egui::{Align, Color32, CornerRadius, Layout, Rect, RichText, Sense, Stroke, Ui, Vec2};
use rustgis_ui_egui::theme::Tokens;
use rustgis_ui_egui::{RustGisApp, icons, tl};
use serde_json::{Value, json};

/// The smallest side of anything a finger taps, in points (Android's guidance is 48).
pub const TARGET: f32 = 48.0;
/// The bar's height: an icon over its label.
const BAR: f32 = 64.0;
/// The most of the window's height a sheet takes, so the map stays in view above it.
const SHEET_SHARE: f32 = 0.55;
/// The room above and below a sheet's rows.
const SHEET_MARGIN: f32 = 4.0;
/// The More sheet's rows with the position hidden.
const MORE_ROWS: usize = 8;

/// The sheet risen over the map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Sheet {
    #[default]
    None,
    Layers,
    /// Capturing a feature where the cross on the map is.
    Capture,
    More,
}

/// The sheet shown, kept in egui's memory: the interface has no state of its own in the app.
fn sheet(ctx: &egui::Context) -> Sheet {
    ctx.data(|d| d.get_temp(egui::Id::unique("touch_sheet"))).unwrap_or_default()
}

fn set_sheet(ctx: &egui::Context, sheet: Sheet) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("touch_sheet"), sheet));
}

/// The touch interface (a `rustgis_ui_egui::Chrome`): the bar, the sheet asked for, and the map
/// in what is left.
pub fn chrome(app: &mut RustGisApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let t = Tokens::get(&ctx);
    let shown = sheet(&ctx);
    let frame = egui::Frame::NONE.fill(t.pane).stroke(Stroke::new(1.0, t.border));
    egui::Panel::bottom("touch_bar").exact_size(BAR).frame(frame).show(ui, |ui| bar(app, ui, &t, shown));
    // Work running off the window's thread (a file being read, an export) shows its progress
    // and Cancel over the bar, where the desktop's status bar would.
    if app.background_busy() {
        egui::Panel::bottom("touch_busy").exact_size(TARGET).frame(frame.inner_margin(egui::Margin::symmetric(12, 0))).show(ui, |ui| {
            // Scrolled sideways when it is wider than the phone (a long address): its Cancel is
            // then reached, not cut off.
            egui::ScrollArea::horizontal().show(ui, |ui| ui.horizontal_centered(|ui| app.background_status(ui)));
        });
    }
    if shown != Sheet::None {
        // As tall as its rows, up to its share of the window: a longer list scrolls.
        let rows = match shown {
            Sheet::Layers => (app.session.map().layers.len() + app.session.map().tiles.len()).max(1),
            // Go to My Position is there only while the position is shown.
            Sheet::More => MORE_ROWS + usize::from(app.locate),
            Sheet::Capture => capture_rows(app, &ctx),
            Sheet::None => 0,
        };
        let spacing = ui.spacing().item_spacing.y;
        let wanted = (rows + 1) as f32 * (TARGET + spacing) + 2.0 * SHEET_MARGIN;
        let height = wanted.min(ui.available_height() * SHEET_SHARE);
        let margin = egui::Margin::symmetric(8, SHEET_MARGIN as i8);
        egui::Panel::bottom("touch_sheet").resizable(false).exact_size(height).frame(frame.inner_margin(margin)).show(
            ui,
            |ui| match shown {
                Sheet::Layers => layers(app, ui, &t),
                Sheet::More => more(app, ui, &t),
                Sheet::Capture => capture(app, ui, &t),
                Sheet::None => {}
            },
        );
    }
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| {
        app.map_view(ui);
        // While capturing, a cross marks the middle of the map: the point goes there, so the
        // map is moved under the cross, which a finger does more exactly than a tap.
        // Where the device is, while its position is shown.
        if let (Some(fix), Some((map, _))) = (app.position, app.map.view_rect) {
            position(app, &ui.painter().with_clip_rect(map), fix, &t);
        }
        if shown == Sheet::Capture
            && let Some((map, _)) = app.map.view_rect
        {
            sketch(app, &ui.painter().with_clip_rect(map), map.center(), &t, &ctx);
            cross(ui.painter(), map.center(), &t);
        }
    });
}

/// Draws where the device is: a dot, in a ring as wide as the position is good to.
fn position(app: &RustGisApp, painter: &egui::Painter, fix: rustgis_ui_egui::rustgis_engine::platform::Fix, t: &Tokens) {
    let Some(at) = app.lonlat_to_screen(fix.lon, fix.lat) else { return };
    // The ground a point of the screen covers where the fix is, not at the middle of the view:
    // on a projection whose scale changes with latitude the two differ.
    let map = app.session.map();
    let here = map.to_map(rustgis_ui_egui::rustgis_engine::geom::Coord::new(fix.lon, fix.lat));
    let metres_a_point = here.map_or(0.0, |at| map.camera.res * map.ground_per_unit(at));
    // A ring wider than the view has no edge in it to show, and an accuracy out of all measure
    // (it is the system's to give) would make one of no size a painter can take.
    let widest = painter.clip_rect().size().length();
    if let Some(radius) = fix.accuracy.filter(|_| metres_a_point > 0.0).map(|m| (m / metres_a_point) as f32)
        && radius > 9.0
        && radius <= widest
    {
        painter.circle(at, radius, t.accent.gamma_multiply(0.15), Stroke::new(1.0, t.accent.gamma_multiply(0.6)));
    }
    painter.circle(at, 8.0, Color32::WHITE, Stroke::NONE);
    painter.circle(at, 6.0, t.accent, Stroke::NONE);
}

/// Moves the map so the device's position is in its middle, under the capture cross. Says so
/// when there is no position yet.
fn go_to_position(app: &mut RustGisApp) {
    match app.position {
        Some(fix) => run(app, "map.goToXY", json!({"lon": fix.lon, "lat": fix.lat})),
        None => app.session.notify(tl!("This device's position isn't known yet."), false),
    }
}

/// The capture cross: dark with a light edge, to show over any map.
fn cross(painter: &egui::Painter, at: egui::Pos2, t: &Tokens) {
    let arm = 14.0;
    for (width, ink) in [(4.0, Color32::WHITE), (2.0, t.accent)] {
        let stroke = Stroke::new(width, ink);
        painter.line_segment([at - Vec2::new(arm, 0.0), at + Vec2::new(arm, 0.0)], stroke);
        painter.line_segment([at - Vec2::new(0.0, arm), at + Vec2::new(0.0, arm)], stroke);
    }
}

/// The bar: one large button for each thing done most, the sheet's own button marked while its
/// sheet is up.
fn bar(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, shown: Sheet) {
    let ctx = ui.ctx().clone();
    let toggle = |to: Sheet| set_sheet(&ctx, if shown == to { Sheet::None } else { to });
    ui.columns(4, |cols| {
        if let [a, b, c, d] = cols {
            if bar_button(a, t, "contents", tl!("Layers"), shown == Sheet::Layers) {
                toggle(Sheet::Layers);
            }
            if bar_button(b, t, "add_data", tl!("Add Data"), false) {
                set_sheet(&ctx, Sheet::None);
                run(app, "ui.addData", Value::Null);
            }
            if bar_button(c, t, "vertices", tl!("Capture"), shown == Sheet::Capture) {
                toggle(Sheet::Capture);
            }
            if bar_button(d, t, "command", tl!("More"), shown == Sheet::More) {
                toggle(Sheet::More);
            }
        }
    });
}

/// One of the bar's buttons, as wide as its column: the icon over the label. Says whether it
/// was tapped.
fn bar_button(ui: &mut Ui, t: &Tokens, icon: &str, label: &str, on: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), BAR), Sense::click());
    let response = response.on_hover_text(label);
    if on || response.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect.shrink(4.0), CornerRadius::same(8), if on { t.checked } else { t.pressed });
    }
    let side = 26.0;
    let at = Rect::from_center_size(rect.center() - Vec2::new(0.0, 9.0), Vec2::splat(side));
    icons::paint(ui.painter(), at, icon, if on { t.accent_text } else { t.icon }, t.accent);
    let ink = if on { t.accent_text } else { t.text };
    ui.painter().text(rect.center() + Vec2::new(0.0, 19.0), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(12.0), ink);
    response.clicked()
}

/// A sheet's heading, with the button that lowers it.
fn heading(ui: &mut Ui, t: &Tokens, title: &str) {
    ui.horizontal(|ui| {
        ui.set_min_height(TARGET);
        ui.label(RichText::new(title).size(17.0).strong().color(t.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_button(ui, t, "close", tl!("Close")) {
                set_sheet(ui.ctx(), Sheet::None);
            }
        });
    });
}

/// A square button of the smallest size a finger taps, showing an icon. Says whether it was
/// tapped.
fn icon_button(ui: &mut Ui, t: &Tokens, icon: &str, hint: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(TARGET), Sense::click());
    let response = response.on_hover_text(hint);
    if response.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect.shrink(4.0), CornerRadius::same(8), t.pressed);
    }
    icons::paint(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(22.0)), icon, t.icon, t.accent);
    response.clicked()
}

/// A row of the Layers sheet: a feature or raster layer by its id, a tile layer by its place.
#[derive(Clone, Copy)]
enum Row {
    Layer(u32),
    Tile(usize),
}

/// The Layers sheet: the active map's layers, then its tile layers, each list top first. A row's
/// eye shows or hides its layer, its name makes it the one acted on, and its last button zooms
/// to it.
fn layers(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    heading(ui, t, tl!("Layers"));
    // Read first: a row's button runs a command, which changes the lists it is drawn from.
    let map = app.session.map();
    let active = app.session.active_layer;
    let picked = app.session.selected_tile();
    let mut rows: Vec<(Row, String, bool, bool)> =
        map.layers.iter().map(|l| (Row::Layer(l.id), l.name.clone(), l.visible, active == Some(l.id))).collect();
    rows.extend(map.tiles.iter().enumerate().map(|(i, tile)| (Row::Tile(i), tile.name.clone(), tile.visible, picked == Some(i))));
    if rows.is_empty() {
        ui.label(RichText::new(tl!("This map has no layers yet. Add Data puts one on it.")).color(t.text_dim));
        ui.add_space(8.0);
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
        for (row, name, visible, chosen) in rows {
            ui.horizontal(|ui| {
                ui.set_min_height(TARGET);
                let eye = if visible { tl!("Hide this layer") } else { tl!("Show this layer") };
                if toggle_button(ui, t, "eye", eye, visible) {
                    match row {
                        Row::Layer(id) => run(app, "layer.visibility", json!({"layer": id, "visible": !visible})),
                        Row::Tile(index) => run(app, "map.setTiles", json!({"index": index, "visible": !visible})),
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, t, "zoom_full", tl!("Zoom to this layer")) {
                        match row {
                            Row::Layer(id) => run(app, "layer.zoomTo", json!({"layer": id})),
                            Row::Tile(index) => run(app, "map.zoomToTiles", json!({"index": index})),
                        }
                    }
                    // The name takes the room between the two buttons, cut short when it is long.
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), TARGET), Sense::click());
                    if chosen {
                        ui.painter().rect_filled(rect, CornerRadius::same(6), t.checked);
                    }
                    let ink = if visible { t.text } else { t.text_disabled };
                    let text = ui.painter().layout_no_wrap(name.clone(), egui::FontId::proportional(15.0), ink);
                    let clip = ui.painter().with_clip_rect(rect.shrink2(Vec2::new(8.0, 0.0)));
                    clip.galley(egui::pos2(rect.min.x + 8.0, rect.center().y - text.size().y / 2.0), text, ink);
                    if response.on_hover_text(name).clicked() {
                        match row {
                            Row::Layer(id) => run(app, "layer.select", json!({"layer": id})),
                            Row::Tile(index) => run(app, "map.selectTiles", json!({"index": index})),
                        }
                    }
                });
            });
        }
    });
}

/// An icon button that shows a state: drawn dim when `on` is false. Says whether it was tapped.
fn toggle_button(ui: &mut Ui, t: &Tokens, icon: &str, hint: &str, on: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(TARGET), Sense::click());
    let response = response.on_hover_text(hint);
    if response.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect.shrink(4.0), CornerRadius::same(8), t.pressed);
    }
    let ink = if on { t.icon } else { t.text_disabled };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), Vec2::splat(22.0)),
        icon,
        ink,
        if on { t.accent } else { Color32::TRANSPARENT },
    );
    response.clicked()
}

/// What the Capture sheet holds between frames: the layer captured into, what is typed for the
/// next feature's fields (by field name), and the vertices placed so far of a line or polygon.
#[derive(Clone, Default)]
struct Form {
    /// The layer a chip chose; `None` follows the active layer. A layer is named here by its
    /// `Layer::instance_id`, not its id: ids start again in each map and each project, and a
    /// choice or a draft must never pass to another map's layer of the same id.
    layer: Option<u64>,
    /// The layer the fields were last shown for: what is typed, and what is drawn, belong to
    /// it alone.
    shown: Option<u64>,
    values: std::collections::BTreeMap<String, String>,
    /// The layer's feature template chosen for the next feature, by name: its defaults fill
    /// the fields the form leaves empty.
    template: Option<String>,
    /// The vertices of the line or polygon being drawn, as longitude and latitude, in order.
    vertices: Vec<(f64, f64)>,
}

impl Form {
    /// The form for the layer `instance`: what was typed or drawn for another layer is dropped,
    /// so one layer's text never goes into another's field of the same name, nor its vertices
    /// into another's feature.
    fn for_layer(mut self, instance: u64) -> Form {
        if self.shown != Some(instance) {
            self.values.clear();
            self.vertices.clear();
            self.template = None;
            self.shown = Some(instance);
        }
        self
    }
}

impl Form {
    /// Places a vertex of the shape being drawn at `at`, unless the last one is there already
    /// (the button tapped twice with the map not moved): a vertex on the one before it adds
    /// nothing to the shape, and two of three in one place make a polygon of no area. Says
    /// whether it was placed.
    fn place(&mut self, at: (f64, f64)) -> bool {
        if self.vertices.last() == Some(&at) {
            return false;
        }
        self.vertices.push(at);
        true
    }
}

fn form(ctx: &egui::Context) -> Form {
    ctx.data(|d| d.get_temp(egui::Id::unique("touch_capture"))).unwrap_or_default()
}

fn set_form(ctx: &egui::Context, form: Form) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("touch_capture"), form));
}

use rustgis_ui_egui::rustgis_engine::data::GeometryType;

/// A layer a feature can be captured into.
#[derive(Clone)]
struct Target {
    id: u32,
    name: String,
    instance: u64,
    kind: GeometryType,
}

/// The map's point, line and polygon layers, top first: what a feature can be captured into.
fn capture_layers(app: &RustGisApp) -> Vec<Target> {
    app.session
        .map()
        .layers
        .iter()
        .filter(|l| matches!(l.geometry_type(), GeometryType::Point | GeometryType::Polyline | GeometryType::Polygon))
        .map(|l| Target { id: l.id, name: l.name.clone(), instance: l.instance_id(), kind: l.geometry_type() })
        .collect()
}

/// The layer the Capture sheet captures into: the one chosen in it while it is still a layer of
/// this map that takes features, else the active layer when it is one, else the top such layer.
fn capture_layer(app: &RustGisApp, form: &Form) -> Option<Target> {
    let layers = capture_layers(app);
    let chosen = form.layer.and_then(|instance| layers.iter().find(|l| l.instance == instance));
    let active = app.session.active_layer.and_then(|id| layers.iter().find(|l| l.id == id));
    chosen.or(active).or(layers.first()).cloned()
}

/// The Capture sheet's rows, for its height: its layers, its fields, where the cross is, and
/// its buttons (two rows of them for a line or polygon).
fn capture_rows(app: &RustGisApp, ctx: &egui::Context) -> usize {
    let Some(target) = capture_layer(app, &form(ctx)) else { return 2 };
    let layer = app.session.map().layer(target.id);
    let fields = layer.map_or(0, |l| l.dataset.fields.len() + usize::from(!l.design.templates.is_empty()));
    fields + if target.kind == GeometryType::Point { 3 } else { 4 } + usize::from(app.locate)
}

/// A value typed for a field, as the command takes it: a number for a field of numbers, a truth
/// value for a yes/no field, the text otherwise. Nothing typed is no value. Text that isn't a
/// number, in a field of numbers, is an error here: the command would store nothing for it, and
/// the feature would be saved without what was typed.
fn typed(text: &str, ty: rustgis_ui_egui::rustgis_engine::data::FieldType) -> Result<Value, ()> {
    use rustgis_ui_egui::rustgis_engine::data::FieldType;
    let text = text.trim();
    if text.is_empty() {
        return Ok(Value::Null);
    }
    match ty {
        FieldType::Integer => text.parse::<i64>().map(Value::from).map_err(|_| ()),
        FieldType::Double => text.parse::<f64>().ok().filter(|n| n.is_finite()).map(Value::from).ok_or(()),
        FieldType::Boolean => match text {
            "true" => Ok(Value::from(true)),
            "false" => Ok(Value::from(false)),
            _ => Err(()),
        },
        // A date the field can't hold is refused by the command, by name.
        FieldType::Text | FieldType::Date | FieldType::DateTime => Ok(Value::from(text)),
    }
}

/// Where the cross is: the middle of the map, as longitude and latitude. `None` where the map's
/// projection has no such place for it (the middle of the view is off the projected world).
fn cross_place(app: &RustGisApp) -> Option<(f64, f64)> {
    let map = app.session.map();
    map.to_lonlat(map.camera.center).map(|at| (at.x, at.y))
}

/// The fewest vertices a feature of `kind` is drawn with.
fn fewest(kind: GeometryType) -> usize {
    match kind {
        GeometryType::Polygon => 3,
        GeometryType::Polyline => 2,
        _ => 1,
    }
}

/// The GeoJSON geometry of a feature of `kind` through `vertices` (longitude and latitude, in
/// order): a point at the first, a line through them, or a polygon whose ring closes on the
/// first. A polygon closed by hand (its last vertex placed on its first) is not closed twice.
/// `None` with fewer than the kind needs.
fn geometry(kind: GeometryType, vertices: &[(f64, f64)]) -> Option<Value> {
    let vertices = match vertices {
        [first, middle @ .., last] if kind == GeometryType::Polygon && first == last && !middle.is_empty() => {
            &vertices[..vertices.len() - 1]
        }
        _ => vertices,
    };
    if vertices.len() < fewest(kind) {
        return None;
    }
    let at = |(lon, lat): &(f64, f64)| json!([lon, lat]);
    match kind {
        GeometryType::Point => vertices.first().map(|v| json!({"type": "Point", "coordinates": at(v)})),
        GeometryType::Polyline => Some(json!({"type": "LineString", "coordinates": vertices.iter().map(at).collect::<Vec<_>>()})),
        GeometryType::Polygon => {
            let mut ring: Vec<Value> = vertices.iter().map(at).collect();
            ring.push(at(vertices.first()?));
            Some(json!({"type": "Polygon", "coordinates": [ring]}))
        }
        GeometryType::None => None,
    }
}

/// Adds a feature to layer `id` with `geometry` and what the form holds, turning editing on for
/// the map first when it is off (and saying so). Says whether it was added. It isn't, and the
/// reason is said, when a field of numbers holds text that isn't one, or when the command
/// refuses.
fn add_feature(app: &mut RustGisApp, id: u32, form: &Form, geometry: Value) -> bool {
    let Some(layer) = app.session.map().layer(id) else { return false };
    let (name, kind) = (layer.name.clone(), layer.geometry_type());
    let mut attributes = serde_json::Map::new();
    let mut not_numbers = Vec::new();
    for field in &layer.dataset.fields {
        let Some(text) = form.values.get(&field.name) else { continue };
        match typed(text, field.ty) {
            Ok(Value::Null) => {}
            Ok(value) => {
                attributes.insert(field.name.clone(), value);
            }
            Err(()) => not_numbers.push(field.name.clone()),
        }
    }
    if !not_numbers.is_empty() {
        let fields = not_numbers.join(", ");
        app.session.notify(
            rustgis_ui_egui::i18n::fmt("Nothing was added. Not a value the field can hold, in: {fields}.", &[("fields", &fields)]),
            true,
        );
        return false;
    }
    if !app.session.map().editing.settings.enabled {
        if app.run_by_user("edit.enable", json!({"on": true})).is_err() {
            return false;
        }
        app.session.notify(tl!("Editing was turned on for this map."), false);
    }
    let mut feature = json!({"layer": id, "geometry": geometry, "attributes": attributes});
    // A template chosen while the layer still has it: one removed since is not asked for.
    let template = form
        .template
        .as_ref()
        .filter(|name| app.session.map().layer(id).is_some_and(|l| l.design.templates.iter().any(|t| &t.name == *name)));
    if let Some(name) = template {
        feature["template"] = json!(name);
    }
    if app.run_by_user("edit.addFeature", feature).is_err() {
        return false;
    }
    let added = match kind {
        GeometryType::Polyline => rustgis_ui_egui::i18n::fmt("Added a line to {layer}.", &[("layer", &name)]),
        GeometryType::Polygon => rustgis_ui_egui::i18n::fmt("Added a polygon to {layer}.", &[("layer", &name)]),
        _ => rustgis_ui_egui::i18n::fmt("Added a point to {layer}.", &[("layer", &name)]),
    };
    app.session.notify(added, false);
    true
}

/// Adds a point to layer `id` where the middle of the map is, with what the form holds. A cross
/// on no place of the map's projection adds none, and says so.
fn add_point(app: &mut RustGisApp, id: u32, form: &Form) -> bool {
    let Some(at) = cross_place(app) else {
        app.session.notify(tl!("Nothing was added: the cross is off the map. Move the map under it."), true);
        return false;
    };
    match geometry(GeometryType::Point, &[at]) {
        Some(point) => add_feature(app, id, form, point),
        None => false,
    }
}

/// The values a field takes from a list, as (what is stored, what is shown): the codes of its
/// coded-value domain, the layer's subtypes for the subtype field, or yes and no for a yes/no
/// field. `None` for a field that is typed into.
fn choices(
    layer: &rustgis_ui_egui::rustgis_engine::map::Layer,
    field: &rustgis_ui_egui::rustgis_engine::data::Field,
) -> Option<Vec<(String, String)>> {
    use rustgis_ui_egui::rustgis_engine::data::FieldType;
    use rustgis_ui_egui::rustgis_engine::map::design::DomainKind;
    let same = |name: &str| name.eq_ignore_ascii_case(&field.name);
    if let Some(subtypes) = layer.design.subtypes.as_ref().filter(|s| same(&s.field)) {
        return Some(subtypes.subtypes.iter().map(|s| (s.code.to_string(), s.name.clone())).collect());
    }
    if let Some(DomainKind::Coded(codes)) = layer.design.domains.iter().find(|d| same(&d.field)).map(|d| &d.kind) {
        // A code of no text is left out: it would be stored as nothing, and its chip would show
        // as chosen whenever nothing is.
        let list: Vec<(String, String)> = codes
            .iter()
            .map(|(code, says)| (code.to_string(), says))
            .filter(|(code, _)| !code.is_empty())
            .map(|(code, says)| if says.trim().is_empty() { (code.clone(), code) } else { (code, says.clone()) })
            .collect();
        // A list with nothing left to offer is no list: the field is typed into.
        if !list.is_empty() {
            return Some(list);
        }
    }
    (field.ty == FieldType::Boolean)
        .then(|| vec![("true".to_string(), tl!("Yes").to_string()), ("false".to_string(), tl!("No").to_string())])
}

/// The limits of a field's range domain, when it has one.
fn range_of(layer: &rustgis_ui_egui::rustgis_engine::map::Layer, field: &str) -> Option<(f64, f64)> {
    use rustgis_ui_egui::rustgis_engine::map::design::DomainKind;
    layer.design.domains.iter().find(|d| d.field.eq_ignore_ascii_case(field)).and_then(|d| match d.kind {
        DomainKind::Range { min, max } => Some((min, max)),
        DomainKind::Coded(_) => None,
    })
}

/// A row of chips, one for each of `list` (what is stored, what is shown), scrolled sideways when
/// it is wider than the sheet. The one whose stored value is `value` is marked; tapping another
/// makes it the value, and tapping the marked one empties the value, so "nothing chosen" shows
/// as no chip marked.
fn chips(ui: &mut Ui, t: &Tokens, salt: impl egui::AsIdSalt, list: &[(String, String)], value: &mut String) {
    egui::ScrollArea::horizontal().id_salt(salt).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (stored, shown) in list {
                let on = stored == value;
                let text = RichText::new(shown).size(15.0).color(if on { t.accent_text } else { t.text });
                let chip = egui::Button::new(text).fill(if on { t.checked } else { t.input }).min_size(Vec2::new(TARGET, TARGET - 8.0));
                if ui.add(chip).clicked() {
                    *value = if on { String::new() } else { stored.clone() };
                }
            }
        });
    });
}

/// Draws the line or polygon being captured over the map: its vertices, the line through them,
/// and a thinner one on to the cross, where the next vertex would go (and, for a polygon, back to
/// the first).
fn sketch(app: &RustGisApp, painter: &egui::Painter, cross: egui::Pos2, t: &Tokens, ctx: &egui::Context) {
    let state = form(ctx);
    let Some(target) = capture_layer(app, &state) else { return };
    if target.kind == GeometryType::Point || state.vertices.is_empty() {
        return;
    }
    // A vertex the view doesn't show (behind the globe) breaks the line there.
    let placed: Vec<Option<egui::Pos2>> = state.vertices.iter().map(|(lon, lat)| app.lonlat_to_screen(*lon, *lat)).collect();
    for pair in placed.windows(2) {
        if let [Some(a), Some(b)] = pair {
            painter.line_segment([*a, *b], Stroke::new(5.0, Color32::WHITE));
            painter.line_segment([*a, *b], Stroke::new(3.0, t.accent));
        }
    }
    let next = Stroke::new(1.5, t.accent);
    if let Some(Some(last)) = placed.last() {
        painter.line_segment([*last, cross], next);
    }
    if target.kind == GeometryType::Polygon
        && placed.len() > 1
        && let Some(Some(first)) = placed.first()
    {
        painter.line_segment([cross, *first], next);
    }
    for at in placed.into_iter().flatten() {
        painter.circle(at, 5.0, Color32::WHITE, Stroke::new(2.0, t.accent));
    }
}

/// A button of the Capture sheet, `width` wide and a finger tall: filled with the accent when it
/// is the one to tap next. Says whether it was tapped.
fn action(ui: &mut Ui, t: &Tokens, label: &str, width: f32, main: bool, enabled: bool) -> bool {
    let text = RichText::new(label).size(16.0).color(if main { t.on_accent } else { t.text });
    let button = egui::Button::new(text).fill(if main { t.accent } else { t.input }).min_size(Vec2::new(width, TARGET));
    ui.add_enabled(enabled, button).clicked()
}

/// The Capture sheet: the layer to capture into, where the cross is, the buttons that capture
/// there, and a field for each of the layer's attributes. A point is added where the cross is.
/// A line or polygon is drawn a vertex at a time, each where the cross is when Add Vertex is
/// tapped, and added by Finish. The sheet stays up after a feature, emptied, for the next one.
fn capture(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    heading(ui, t, tl!("Capture"));
    let ctx = ui.ctx().clone();
    let mut state = form(&ctx);
    let layers = capture_layers(app);
    let Some(target) = capture_layer(app, &state) else {
        ui.label(RichText::new(tl!("This map has no layer to capture into. Add Data puts one on it.")).color(t.text_dim));
        ui.add_space(8.0);
        return;
    };
    state = state.for_layer(target.instance);
    // A template chosen and since removed from the layer is let go, and said, before a button can
    // add a feature without the defaults the form still seemed to promise: nothing is added on
    // the frame that finds it, so the next tap is made knowing.
    let on_layer = |name: &String| app.session.map().layer(target.id).is_some_and(|l| l.design.templates.iter().any(|t| &t.name == name));
    let stale = state.template.clone().filter(|name| !on_layer(name));
    if let Some(gone) = &stale {
        state.template = None;
        app.session.notify(rustgis_ui_egui::i18n::fmt("The template {name} is no longer on this layer.", &[("name", gone)]), false);
    }
    let fresh = stale.is_none();
    // The buttons stay in reach above the fields, however many the layer has.
    let cross = cross_place(app);
    let place = match cross {
        Some((lon, lat)) => {
            rustgis_ui_egui::i18n::fmt("At the cross: {lon}, {lat}", &[("lon", &format!("{lon:.6}")), ("lat", &format!("{lat:.6}"))])
        }
        None => tl!("The cross is off the map: move the map under it.").to_string(),
    };
    ui.label(RichText::new(place).size(13.0).color(t.text_dim));
    let width = ui.available_width();
    // With the device's position shown, one tap puts the cross on it.
    if app.locate && action(ui, t, tl!("Go to My Position"), width, false, app.position.is_some()) {
        go_to_position(app);
    }
    if target.kind == GeometryType::Point {
        if action(ui, t, tl!("Add Point Here"), width, true, true) && fresh && add_point(app, target.id, &state) {
            state.values.clear();
        }
    } else {
        let placed = state.vertices.len();
        // Enough, once the vertices make the layer's kind of shape.
        let enough = geometry(target.kind, &state.vertices).is_some();
        if action(ui, t, tl!("Add Vertex Here"), width, !enough, true) {
            match cross {
                Some(at) if state.place(at) => {}
                Some(_) => app.session.notify(tl!("No vertex was added: the cross is where the last one is. Move the map under it."), true),
                None => app.session.notify(tl!("No vertex was added: the cross is off the map. Move the map under it."), true),
            }
        }
        ui.horizontal(|ui| {
            let gap = ui.spacing().item_spacing.x;
            let third = (width - 2.0 * gap) / 3.0;
            if action(ui, t, tl!("Undo Vertex"), third, false, placed > 0) {
                state.vertices.pop();
            }
            if action(ui, t, tl!("Start Over"), third, false, placed > 0) {
                state.vertices.clear();
            }
            // Finish is the one to tap once the shape has enough vertices.
            let finish = match target.kind {
                GeometryType::Polygon => tl!("Finish Polygon"),
                _ => tl!("Finish Line"),
            };
            if action(ui, t, finish, third, enough, enough)
                && fresh
                && let Some(shape) = geometry(target.kind, &state.vertices)
                && add_feature(app, target.id, &state, shape)
            {
                state.values.clear();
                state.vertices.clear();
            }
        });
    }
    egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
        // The layer: one chip for each layer that takes features, in a row that scrolls sideways.
        egui::ScrollArea::horizontal().id_salt("touch_capture_layers").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(TARGET);
                for layer in &layers {
                    let on = layer.id == target.id;
                    let text = RichText::new(&layer.name).size(15.0).color(if on { t.accent_text } else { t.text });
                    let chip = egui::Button::new(text).fill(if on { t.checked } else { t.input }).min_size(Vec2::new(TARGET, TARGET - 8.0));
                    // The next frame shows the chosen layer's fields, empty (`Form::for_layer`).
                    if ui.add(chip).clicked() && !on {
                        state.layer = Some(layer.instance);
                    }
                }
            });
        });
        // Read first: the rows change the form, not the layer.
        let (fields, templates, lists): (Vec<_>, Vec<String>, Vec<Option<Vec<(String, String)>>>) = match app.session.map().layer(target.id)
        {
            Some(l) => (
                l.dataset.fields.clone(),
                l.design.templates.iter().map(|template| template.name.clone()).collect(),
                l.dataset.fields.iter().map(|f| choices(l, f)).collect(),
            ),
            None => Default::default(),
        };
        // The layer's feature templates: one fills in its defaults for what the form leaves empty.
        if !templates.is_empty() {
            ui.horizontal(|ui| {
                ui.set_min_height(TARGET);
                ui.add_sized([110.0, TARGET], egui::Label::new(RichText::new(tl!("Template")).size(14.0).color(t.text_dim)).truncate());
                let picked = state.template.clone().unwrap_or_default();
                let list: Vec<(String, String)> = templates.iter().map(|name| (name.clone(), name.clone())).collect();
                let mut chosen = picked.clone();
                chips(ui, t, "touch_capture_templates", &list, &mut chosen);
                if chosen != picked {
                    state.template = Some(chosen).filter(|name| !name.is_empty());
                }
            });
        }
        for (field, list) in fields.iter().zip(lists) {
            use rustgis_ui_egui::rustgis_engine::data::FieldType;
            let label = field.alias.clone().filter(|a| !a.trim().is_empty()).unwrap_or_else(|| field.name.clone());
            ui.horizontal(|ui| {
                ui.set_min_height(TARGET);
                ui.add_sized([110.0, TARGET], egui::Label::new(RichText::new(&label).size(14.0).color(t.text_dim)).truncate());
                let value = state.values.entry(field.name.clone()).or_default();
                if let Some(list) = list {
                    // A list of values: one is tapped, and tapped again to leave the field empty.
                    chips(ui, t, ("touch_capture_values", &field.name), &list, value);
                } else {
                    let range = app.session.map().layer(target.id).and_then(|l| range_of(l, &field.name));
                    let hint = match (field.ty, range) {
                        (_, Some((min, max))) => rustgis_ui_egui::i18n::fmt(
                            "a number from {min} to {max}",
                            &[("min", &min.to_string()), ("max", &max.to_string())],
                        ),
                        (FieldType::Integer | FieldType::Double, None) => tl!("a number").to_string(),
                        (FieldType::Date, None) => tl!("a date, as 2026-03-05").to_string(),
                        (FieldType::DateTime, None) => tl!("a date and time").to_string(),
                        _ => String::new(),
                    };
                    let edit = egui::TextEdit::singleline(value)
                        .hint_text(hint)
                        .font(egui::FontId::proportional(15.0))
                        .margin(egui::Margin::symmetric(8, 10));
                    ui.add_sized([ui.available_width(), TARGET - 6.0], edit);
                }
            });
        }
    });
    // The layer is kept only when a chip chose it: until then the sheet follows the active layer.
    set_form(&ctx, state);
}

/// The More sheet: what is done now and then, each a row as tall as a finger.
fn more(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    heading(ui, t, tl!("More"));
    let rows: [(&str, &str, &str); 6] = [
        ("zoom_full", tl!("Full Extent"), "map.zoomFull"),
        ("open", tl!("Open Project"), "ui.open"),
        ("save", tl!("Save Project"), "ui.save"),
        ("new", tl!("New Project"), "project.new"),
        ("undo", tl!("Undo"), "edit.undo"),
        ("redo", tl!("Redo"), "edit.redo"),
    ];
    egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
        for (icon, label, command) in rows {
            if row(ui, t, icon, label) {
                // Open and New ask first when the project has unsaved changes: the front end's
                // own question, as for the ribbon's buttons.
                set_sheet(ui.ctx(), Sheet::None);
                run(app, command, Value::Null);
            }
        }
        // The device's position, where the shell can report one (it asks the user's leave first).
        let locate = if app.locate { tl!("Hide My Position") } else { tl!("Show My Position") };
        if row(ui, t, "explore", locate) {
            set_sheet(ui.ctx(), Sheet::None);
            app.locate = !app.locate;
        }
        if app.locate && row(ui, t, "zoom_full", tl!("Go to My Position")) {
            set_sheet(ui.ctx(), Sheet::None);
            go_to_position(app);
        }
        // The whole of RustGIS is still there, for a tablet or a job the touch interface has no
        // button for yet. The Project page's Start brings this interface back at the next start.
        if row(ui, t, "pane", tl!("Use the Desktop Interface")) {
            set_sheet(ui.ctx(), Sheet::None);
            app.chrome = None;
            app.session.notify(tl!("The desktop interface is on until RustGIS is started again."), false);
        }
    });
}

/// A row of a sheet's list: an icon and a label, as wide as the sheet. Says whether it was tapped.
fn row(ui: &mut Ui, t: &Tokens, icon: &str, label: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), TARGET), Sense::click());
    if response.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, CornerRadius::same(6), t.pressed);
    }
    icons::paint(
        ui.painter(),
        Rect::from_center_size(egui::pos2(rect.min.x + TARGET / 2.0, rect.center().y), Vec2::splat(22.0)),
        icon,
        t.icon,
        t.accent,
    );
    ui.painter().text(
        egui::pos2(rect.min.x + TARGET + 4.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(15.0),
        t.text,
    );
    response.clicked()
}

/// Runs a command the user tapped for. What it refuses, the front end says in the window itself,
/// as it does for the ribbon's buttons.
fn run(app: &mut RustGisApp, command: &str, params: Value) {
    let _ = app.run_by_user(command, params);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Draws a frame of the interface for a phone's window with `shown` up.
    fn frame(app: &mut RustGisApp, ctx: &egui::Context, shown: Sheet) {
        set_sheet(ctx, shown);
        let input =
            egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(411.0, 914.0))), ..Default::default() };
        ctx.run_ui(input, |ui| chrome(app, ui)).drop_without_applying_deltas();
    }

    #[test]
    fn every_sheet_draws_for_the_sample_and_for_an_empty_map() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        for shown in [Sheet::None, Sheet::Layers, Sheet::Capture, Sheet::More] {
            frame(&mut app, &ctx, shown);
            assert_eq!(sheet(&ctx), shown);
        }
        // No layers: the sheet says so instead of listing.
        let ids: Vec<u32> = app.session.map().layers.iter().map(|l| l.id).collect();
        for id in ids {
            run(&mut app, "layer.remove", json!({"layer": id}));
        }
        assert!(app.session.map().layers.is_empty());
        frame(&mut app, &ctx, Sheet::Layers);
    }

    #[test]
    fn a_map_with_a_tile_layer_draws_its_sheet_and_takes_the_rows_commands() {
        let mut app = RustGisApp::with_sample();
        run(&mut app, "map.addTiles", json!({"url": "https://tiles.example.com/{z}/{x}/{y}.png", "name": "Imagery"}));
        assert_eq!(app.session.map().tiles.len(), 1);
        frame(&mut app, &egui::Context::default(), Sheet::Layers);
        run(&mut app, "map.setTiles", json!({"index": 0, "visible": false}));
        assert!(!app.session.map().tiles[0].visible);
    }

    #[test]
    fn a_project_with_unsaved_changes_is_kept_until_the_user_answers() {
        let mut app = RustGisApp::with_sample();
        app.session.mark_saved();
        let layers = app.session.map().layers.len();
        assert!(layers > 0);
        // Unsaved: New Project waits for the front end's question, and the project stays.
        let first = app.session.map().layers.first().map(|l| l.id);
        run(&mut app, "layer.rename", json!({"layer": first, "name": "Renamed"}));
        assert!(app.session.dirty());
        run(&mut app, "project.new", Value::Null);
        assert_eq!(app.session.map().layers.len(), layers);
    }

    #[test]
    fn a_saved_project_is_replaced_without_a_question() {
        let mut app = RustGisApp::with_sample();
        app.session.mark_saved();
        run(&mut app, "project.new", Value::Null);
        assert!(app.session.map().layers.is_empty());
    }

    /// The sample's layer of `kind`, made the active one, as the sheet resolves it.
    fn target(app: &mut RustGisApp, kind: GeometryType) -> Target {
        let id = app.session.map().layers.iter().find(|l| l.geometry_type() == kind).map(|l| l.id).unwrap();
        run(app, "layer.select", json!({"layer": id}));
        let target = capture_layer(app, &Form::default()).unwrap();
        assert_eq!(target.id, id);
        target
    }

    fn count(app: &RustGisApp, id: u32) -> usize {
        app.session.map().layer(id).unwrap().dataset.features.len()
    }

    #[test]
    fn a_point_is_captured_where_the_map_is_centred_with_what_the_form_holds() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        // The sample's Cities are points.
        let cities = target(&mut app, GeometryType::Point);
        assert_eq!(cities.name, "Cities");
        let before = count(&app, cities.id);
        let layer = app.session.map().layer(cities.id).unwrap();
        let text = layer
            .dataset
            .fields
            .iter()
            .find(|f| f.ty == rustgis_ui_egui::rustgis_engine::data::FieldType::Text)
            .map(|f| f.name.clone())
            .unwrap();
        let mut state = Form { layer: Some(cities.instance), ..Form::default() };
        state.values.insert(text.clone(), "Field Camp".into());
        // With editing off, capturing turns it on, and says so.
        run(&mut app, "edit.enable", json!({"on": false}));
        assert!(!app.session.map().editing.settings.enabled);
        let mark = app.session.notice_mark();
        assert!(add_point(&mut app, cities.id, &state));
        assert!(app.session.map().editing.settings.enabled);
        assert!(app.session.notices_since(mark).iter().all(|n| !n.error));
        assert_eq!(app.session.notices_since(mark).len(), 2);
        assert_eq!(count(&app, cities.id), before + 1);
        // The sheet draws with the new point's layer, and for a map with nothing to capture into.
        frame(&mut app, &ctx, Sheet::Capture);
        let ids: Vec<u32> = app.session.map().layers.iter().map(|l| l.id).collect();
        for id in ids {
            run(&mut app, "layer.remove", json!({"layer": id}));
        }
        assert!(capture_layer(&app, &Form { layer: Some(cities.instance), ..Form::default() }).is_none());
        frame(&mut app, &ctx, Sheet::Capture);
    }

    #[test]
    fn a_line_and_a_polygon_are_captured_through_their_vertices() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        // Too few vertices make no geometry; enough make the kind's own.
        let square = [(10.0, 10.0), (11.0, 10.0), (11.0, 11.0), (10.0, 11.0)];
        assert_eq!(geometry(GeometryType::Polyline, &square[..1]), None);
        assert_eq!(geometry(GeometryType::Polygon, &square[..2]), None);
        let line = geometry(GeometryType::Polyline, &square[..2]).unwrap();
        assert_eq!(line, json!({"type": "LineString", "coordinates": [[10.0, 10.0], [11.0, 10.0]]}));
        // A polygon's ring closes on its first vertex.
        let ring = geometry(GeometryType::Polygon, &square).unwrap();
        assert_eq!(ring["coordinates"][0].as_array().unwrap().len(), 5);
        assert_eq!(ring["coordinates"][0][0], ring["coordinates"][0][4]);
        for (kind, shape, word) in [(GeometryType::Polyline, line, "line"), (GeometryType::Polygon, ring, "polygon")] {
            let layer = target(&mut app, kind);
            let before = count(&app, layer.id);
            let mark = app.session.notice_mark();
            assert!(add_feature(&mut app, layer.id, &Form::default(), shape));
            assert_eq!(count(&app, layer.id), before + 1);
            let said: Vec<_> = app.session.notices_since(mark).iter().map(|n| n.text.clone()).collect();
            assert!(said.iter().any(|text| text.contains(word)), "{said:?}");
            // The sheet draws for the layer with a shape half drawn, its sketch over the map.
            set_form(&ctx, Form { vertices: square[..3].to_vec(), shown: Some(layer.instance), ..Form::default() });
            frame(&mut app, &ctx, Sheet::Capture);
            assert_eq!(form(&ctx).vertices.len(), 3);
        }
        // Closed by hand, on its first vertex: the ring is the same, and three in all are too few.
        let by_hand = [square[0], square[1], square[2], square[3], square[0]];
        assert_eq!(geometry(GeometryType::Polygon, &by_hand), geometry(GeometryType::Polygon, &square));
        assert_eq!(geometry(GeometryType::Polygon, &[square[0], square[1], square[0]]), None);
        // A line may come back to where it began.
        assert!(geometry(GeometryType::Polyline, &[square[0], square[1], square[0]]).is_some());
        // A vertex where the last one is isn't placed; one anywhere else is.
        let mut drawing = Form::default();
        assert!(drawing.place(square[0]));
        assert!(!drawing.place(square[0]));
        assert!(drawing.place(square[1]));
        assert_eq!(drawing.vertices, square[..2]);
        // A shape of the wrong kind for its layer is refused, and said.
        let cities = target(&mut app, GeometryType::Point);
        let before = count(&app, cities.id);
        let mark = app.session.notice_mark();
        assert!(!add_feature(&mut app, cities.id, &Form::default(), geometry(GeometryType::Polyline, &square).unwrap()));
        assert_eq!(count(&app, cities.id), before);
        assert_eq!(app.session.notices_since(mark).iter().filter(|n| n.error).count(), 1);
    }

    #[test]
    fn the_sheet_follows_the_active_layer_until_a_chip_chooses_another() {
        let mut app = RustGisApp::with_sample();
        let cities = target(&mut app, GeometryType::Point);
        let rivers = target(&mut app, GeometryType::Polyline);
        // Nothing chosen: the active layer, whichever it is, and it is followed as it changes.
        for layer in [&cities, &rivers] {
            run(&mut app, "layer.select", json!({"layer": layer.id}));
            assert_eq!(capture_layer(&app, &Form::default()).map(|l| l.id), Some(layer.id));
        }
        // Drawing the sheet doesn't choose: the next frame still follows the active layer.
        let ctx = egui::Context::default();
        frame(&mut app, &ctx, Sheet::Capture);
        assert_eq!(form(&ctx).layer, None);
        // A chip's choice wins over the active layer.
        run(&mut app, "layer.select", json!({"layer": cities.id}));
        assert_eq!(capture_layer(&app, &Form { layer: Some(rivers.instance), ..Form::default() }).map(|l| l.id), Some(rivers.id));
        // A choice made for another map's or project's layer is no choice here, whatever its id.
        assert_eq!(capture_layer(&app, &Form { layer: Some(u64::MAX), ..Form::default() }).map(|l| l.id), Some(cities.id));
        // The sample opened again: its Cities is another layer, though its id is the same.
        app.session.mark_saved();
        run(&mut app, "project.sample", Value::Null);
        let again = capture_layers(&app).into_iter().find(|l| l.id == cities.id).map(|l| l.instance);
        assert!(again.is_some() && again != Some(cities.instance), "{again:?}");
    }

    #[test]
    fn text_in_a_field_of_numbers_keeps_the_point_from_being_added_and_is_said() {
        use rustgis_ui_egui::rustgis_engine::data::FieldType;
        let mut app = RustGisApp::with_sample();
        let id = target(&mut app, GeometryType::Point).id;
        let layer = app.session.map().layer(id).unwrap();
        let before = layer.dataset.features.len();
        let number =
            layer.dataset.fields.iter().find(|f| matches!(f.ty, FieldType::Integer | FieldType::Double)).map(|f| f.name.clone()).unwrap();
        let mut state = Form::default();
        state.values.insert(number.clone(), "many".into());
        let mark = app.session.notice_mark();
        assert!(!add_point(&mut app, id, &state));
        assert_eq!(count(&app, id), before);
        let said: Vec<_> = app.session.notices_since(mark).iter().filter(|n| n.error).collect();
        assert_eq!(said.len(), 1);
        assert!(said[0].text.contains(&number), "{}", said[0].text);
        // A number there goes in, with no error.
        state.values.insert(number, "12".into());
        let mark = app.session.notice_mark();
        assert!(add_point(&mut app, id, &state));
        assert!(app.session.notices_since(mark).iter().all(|n| !n.error));
    }

    #[test]
    fn a_fields_list_range_and_template_come_from_the_layers_design() {
        use rustgis_ui_egui::rustgis_engine::data::{Field, FieldType};
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        let cities = target(&mut app, GeometryType::Point);
        let fields = app.session.map().layer(cities.id).unwrap().dataset.fields.clone();
        let text = fields.iter().find(|f| f.ty == FieldType::Text).unwrap().clone();
        let number = fields.iter().find(|f| matches!(f.ty, FieldType::Integer | FieldType::Double)).unwrap().clone();
        // No design yet: both are typed into. A yes/no field always has its two values.
        let layer = app.session.map().layer(cities.id).unwrap();
        assert_eq!(choices(layer, &text), None);
        assert_eq!(range_of(layer, &number.name), None);
        let yes_no = choices(layer, &Field::new("surveyed", FieldType::Boolean)).unwrap();
        assert_eq!(yes_no.iter().map(|(stored, _)| stored.as_str()).collect::<Vec<_>>(), ["true", "false"]);
        // A coded domain gives the field a list, shown by description; a range gives its limits.
        let coded = json!({"layer": cities.id, "field": text.name, "coded": [["cap", "Capital"], ["town", ""]], "force": true});
        run(&mut app, "data.domain", coded);
        run(&mut app, "data.domain", json!({"layer": cities.id, "field": number.name, "min": 0, "max": 10, "force": true}));
        let layer = app.session.map().layer(cities.id).unwrap();
        assert_eq!(choices(layer, &text), Some(vec![("cap".into(), "Capital".into()), ("town".into(), "town".into())]));
        assert_eq!(range_of(layer, &number.name), Some((0.0, 10.0)));
        assert_eq!(choices(layer, &number), None);
        // A template's defaults fill what the form leaves empty.
        run(&mut app, "edit.template", json!({"layer": cities.id, "name": "Capital", "defaults": {text.name.clone(): "cap"}}));
        let state = Form { template: Some("Capital".into()), ..Form::default() };
        assert!(add_point(&mut app, cities.id, &state));
        let layer = app.session.map().layer(cities.id).unwrap();
        let column = layer.dataset.fields.iter().position(|f| f.name == text.name).unwrap();
        assert_eq!(layer.dataset.features.last().unwrap().attrs[column].to_string(), "cap");
        // A template the layer no longer has is not asked for: the point is still added.
        let before = count(&app, cities.id);
        assert!(add_point(&mut app, cities.id, &Form { template: Some("Gone".into()), ..Form::default() }));
        assert_eq!(count(&app, cities.id), before + 1);
        // A value outside the list is refused by the command, and said.
        let mut state = Form::default();
        state.values.insert(text.name.clone(), "village".into());
        let mark = app.session.notice_mark();
        assert!(!add_point(&mut app, cities.id, &state));
        assert!(app.session.notices_since(mark).iter().any(|n| n.error));
        // A code of no text is not offered: it would show as chosen whenever nothing is.
        run(
            &mut app,
            "data.domain",
            json!({"layer": cities.id, "field": text.name, "coded": [["", "None"], ["cap", "Capital"]], "force": true}),
        );
        let layer = app.session.map().layer(cities.id).unwrap();
        assert_eq!(choices(layer, &text), Some(vec![("cap".into(), "Capital".into())]));
        // With no code left to offer, the field is typed into, as one without a list is.
        // (The command itself drops a code of no text; a design read from a project may hold one.)
        use rustgis_ui_egui::rustgis_engine::data::Value as Stored;
        use rustgis_ui_egui::rustgis_engine::map::design::{Domain, DomainKind};
        let mut copy = app.session.map().layer(cities.id).unwrap().clone();
        copy.design.domains =
            vec![Domain { field: text.name.clone(), kind: DomainKind::Coded(vec![(Stored::Text(String::new()), "None".into())]) }];
        assert_eq!(choices(&copy, &text), None);
        // A template chosen and since removed is let go by the sheet, with a notice.
        set_form(&ctx, Form { shown: Some(cities.instance), template: Some("Gone".into()), ..Form::default() });
        let mark = app.session.notice_mark();
        frame(&mut app, &ctx, Sheet::Capture);
        assert_eq!(form(&ctx).template, None);
        assert!(app.session.notices_since(mark).iter().any(|n| n.text.contains("Gone")));
        // The sheet draws the lists, the range hint and the templates.
        set_form(&ctx, Form { shown: Some(cities.instance), template: Some("Capital".into()), ..Form::default() });
        frame(&mut app, &ctx, Sheet::Capture);
        assert_eq!(form(&ctx).template.as_deref(), Some("Capital"));
    }

    #[test]
    fn the_devices_position_is_drawn_and_the_map_goes_to_it() {
        use rustgis_ui_egui::rustgis_engine::platform::Fix;
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        // No position yet: going to it says so, and the map stays.
        let before = cross_place(&app);
        let mark = app.session.notice_mark();
        go_to_position(&mut app);
        assert_eq!(cross_place(&app), before);
        assert_eq!(app.session.notices_since(mark).len(), 1);
        // With one, the map's middle (the capture cross) goes to it.
        app.locate = true;
        app.position = Some(Fix { lon: 30.0, lat: -2.0, accuracy: Some(5.0) });
        go_to_position(&mut app);
        let (lon, lat) = cross_place(&app).unwrap();
        assert!((lon - 30.0).abs() < 1e-6 && (lat + 2.0).abs() < 1e-6, "{lon} {lat}");
        // Every sheet draws with the position on the map, its ring too (a wide one, to be drawn).
        // An accuracy out of all measure draws too, without its ring.
        for accuracy in [500_000.0, 1e300, f64::INFINITY] {
            app.position = Some(Fix { lon: 30.0, lat: -2.0, accuracy: Some(accuracy) });
            for shown in [Sheet::None, Sheet::Capture, Sheet::More] {
                frame(&mut app, &ctx, shown);
            }
        }
    }

    #[test]
    fn what_is_typed_for_one_layer_is_dropped_for_another() {
        let mut state = Form::default().for_layer(1);
        state.values.insert("name".into(), "Camp".into());
        // The same layer keeps it; another drops it.
        state.vertices.push((1.0, 2.0));
        state.template = Some("Camp".into());
        let state = state.for_layer(1);
        assert_eq!(state.template.as_deref(), Some("Camp"));
        assert_eq!((state.values.len(), state.vertices.len()), (1, 1));
        let other = state.for_layer(2);
        assert!(other.values.is_empty() && other.vertices.is_empty() && other.template.is_none());
    }

    #[test]
    fn what_is_typed_becomes_the_fields_kind_of_value() {
        use rustgis_ui_egui::rustgis_engine::data::FieldType;
        assert_eq!(typed("  ", FieldType::Text), Ok(Value::Null));
        assert_eq!(typed(" 42 ", FieldType::Integer), Ok(json!(42)));
        assert_eq!(typed("4.5", FieldType::Double), Ok(json!(4.5)));
        assert_eq!(typed("true", FieldType::Boolean), Ok(json!(true)));
        assert_eq!(typed("false", FieldType::Boolean), Ok(json!(false)));
        assert_eq!(typed("soon", FieldType::Date), Ok(json!("soon")));
        // Not what the field holds: an error, not a value the command would store as nothing.
        assert_eq!(typed("many", FieldType::Integer), Err(()));
        assert_eq!(typed("4.5", FieldType::Integer), Err(()));
        assert_eq!(typed("inf", FieldType::Double), Err(()));
        assert_eq!(typed("yes", FieldType::Boolean), Err(()));
    }

    #[test]
    fn a_refused_command_is_said() {
        let mut app = RustGisApp::with_sample();
        let before = app.session.notice_mark();
        run(&mut app, "layer.zoomTo", json!({"layer": "no such layer"}));
        // Once: the front end reports a refusal itself.
        assert_eq!(app.session.notices_since(before).iter().filter(|n| n.error).count(), 1);
    }
}
