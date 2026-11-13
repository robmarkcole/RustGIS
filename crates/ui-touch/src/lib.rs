//! The touch interface for phones (#1902, phase 2): the map with a bar of large buttons under it
//! and sheets that rise over it, in place of the desktop's ribbon and panes.
//!
//! It is drawn around the egui front end's own map view and runs the same commands: a shell
//! turns it on with `app.chrome = Some(rustgis_ui_touch::chrome)`. Dialogs, notices and the
//! Project page stay the front end's.
//!
//! What is here: the map, the bar (Layers, Add Data, Full Extent, More), the Layers sheet (show,
//! hide, zoom to, the active layer) and the More sheet (project, undo, the desktop interface).
//! Not yet: the feature form, digitizing, and text input that suits a soft keyboard.
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
/// The More sheet's rows.
const MORE_ROWS: usize = 6;

/// The sheet risen over the map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Sheet {
    #[default]
    None,
    Layers,
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
            Sheet::More => MORE_ROWS,
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
                Sheet::None => {}
            },
        );
    }
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| app.map_view(ui));
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
            if bar_button(c, t, "zoom_full", tl!("Full Extent"), false) {
                run(app, "map.zoomFull", Value::Null);
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

/// The More sheet: what is done now and then, each a row as tall as a finger.
fn more(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    heading(ui, t, tl!("More"));
    let rows: [(&str, &str, &str); 5] = [
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
        for shown in [Sheet::None, Sheet::Layers, Sheet::More] {
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

    #[test]
    fn a_refused_command_is_said() {
        let mut app = RustGisApp::with_sample();
        let before = app.session.notice_mark();
        run(&mut app, "layer.zoomTo", json!({"layer": "no such layer"}));
        // Once: the front end reports a refusal itself.
        assert_eq!(app.session.notices_since(before).iter().filter(|n| n.error).count(), 1);
    }
}
