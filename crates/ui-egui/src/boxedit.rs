//! Image-coordinate rectangle manipulation. Previews never mutate the dataset: a release
//! commits one command; Escape, source edits and context changes discard the gesture.
use crate::{
    RustGisApp,
    mapview::{lonlat_to_screen, screen_to_lonlat},
};
use egui::{Color32, Pos2, Rect, Stroke, Ui, vec2};
use rustgis_engine::{
    Tool,
    cmd::annotation::{BoxSpec, Frame, check_layer, class_colors, default_class_color},
    data::Feature,
};
use serde_json::json;

#[derive(Default)]
pub(crate) struct State {
    context: Option<(u64, usize, Option<u32>)>,
    map_name: Option<String>,
    reference: Option<u32>,
    class: String,
    three_clicks: bool,
    panning: bool,
    points: Vec<[f64; 2]>,
    sketch_frame: Option<Frame>,
    gesture: Option<Gesture>,
    preview: Option<BoxSpec>,
    error: Option<String>,
    classes: Classes,
}
/// Only rescan class suggestions after source rows, schema or layer identity changes.
#[derive(Default)]
struct Classes {
    key: Option<(u64, usize, u32, Option<usize>)>,
    rows: std::sync::Weak<Vec<Feature>>,
    values: std::collections::BTreeSet<String>,
    limited: bool,
}
fn refresh_classes(app: &mut RustGisApp) {
    let Some(l) = app.session.active_layer.and_then(|id| app.session.map().layer(id)) else {
        app.boxes.classes = Classes::default();
        return;
    };
    let fi = l.dataset.field_index("class");
    let key = (app.session.opened, app.session.project.active_map, l.id, fi);
    let rows = l.dataset.features.allocation_key();
    let c = &mut app.boxes.classes;
    if c.key == Some(key) && c.rows.ptr_eq(&rows) {
        return;
    }
    c.key = Some(key);
    c.rows = rows;
    c.limited = l.dataset.features.len() > 10_000;
    c.values = fi
        .map(|fi| {
            l.dataset
                .features
                .iter()
                .take(10_000)
                .filter_map(|f| match f.attrs.get(fi) {
                    Some(rustgis_engine::data::Value::Text(c)) if !c.is_empty() => Some(c.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
}
fn supported(app: &RustGisApp) -> bool {
    app.session.map().camera.pitch == 0. && !app.session.map().projection.is_globe() && app.session.map().body == "earth"
}
fn ready(app: &RustGisApp) -> bool {
    app.session.active_layer.is_some_and(|id| {
        check_layer(&app.session, id).is_ok()
            && app.session.map().layer(id).is_some_and(|l| app.session.map().draws_at(l, app.session.scale()))
    })
}
struct Gesture {
    layer: u32,
    row: Option<usize>,
    rows: std::sync::Weak<Vec<Feature>>,
    original: BoxSpec,
    start: [f64; 2],
    handle: Handle,
}
#[derive(Clone, Copy)]
enum Handle {
    New,
    Move,
    Rotate,
    Resize(f64, f64),
}

pub(crate) fn cancel(state: &mut State) {
    state.panning = false;
    state.gesture = None;
    state.preview = None;
    state.points.clear();
    state.sketch_frame = None;
    state.error = None;
}
pub(crate) fn follow(app: &mut RustGisApp) {
    let context = (app.session.opened, app.session.project.active_map, app.session.active_layer);
    let map_name = app.session.map().name.clone();
    if app.boxes.context != Some(context)
        || app.boxes.map_name.as_deref() != Some(map_name.as_str())
        || app.session.tool != Tool::RotatedBox
        || app.session.active_layout.is_some()
        || app.mb.shown()
        || app.ws.catalog_view
        || app.ui.backstage
        || !supported(app)
        || !ready(app)
        || !app.session.map().editing.settings.enabled
    {
        app.boxes.panning = false;
        app.boxes.gesture = None;
        app.boxes.preview = None;
        app.boxes.points.clear();
        app.boxes.sketch_frame = None;
        if app.boxes.context.is_some_and(|c| c.0 != context.0 || c.1 != context.1) {
            app.boxes.reference = None;
        }
        app.boxes.context = Some(context);
    }
    app.boxes.map_name = Some(map_name);
    if app.boxes.sketch_frame.as_ref().is_some_and(|f| frame(app).as_ref() != Ok(f)) {
        app.boxes.points.clear();
        app.boxes.sketch_frame = None;
        app.boxes.preview = None;
    }
    if let Some(g) = &app.boxes.gesture {
        let valid = app.session.map().editing.settings.enabled
            && app.session.map().layer(g.layer).is_some_and(|l| {
                app.session.map().draws_at(l, app.session.scale())
                    && !l.is_streamed()
                    && g.rows.ptr_eq(&l.dataset.features.allocation_key())
                    && g.row.is_none_or(|r| l.selection == [r] && l.drawn_rows(app.session.scale()).get(r) == Some(&true))
                    && (g.row.is_some() || frame(app).as_ref() == Ok(&g.original.frame))
            });
        if !valid {
            app.boxes.gesture = None;
            app.boxes.preview = None;
        }
    }
}
fn selected(app: &RustGisApp) -> Result<(u32, usize, BoxSpec), String> {
    let id = app.session.active_layer.ok_or("select an annotation layer")?;
    let l = app.session.map().layer(id).ok_or("no layer")?;
    if !supported(app) || !ready(app) || !app.session.map().editing.settings.enabled {
        return Err("select a visible, editable annotation layer".into());
    }
    let [row] = l.selection.as_slice() else {
        return Err("select one box to edit its dimensions".into());
    };
    if l.drawn_rows(app.session.scale()).get(*row) != Some(&true) {
        return Err(tl!("The selected box is hidden by a filter.").into());
    }
    Ok((id, *row, BoxSpec::from_row(&l.dataset, *row)?))
}
fn frame(app: &RustGisApp) -> Result<Frame, String> {
    app.boxes.reference.map_or_else(|| Ok(Frame::default()), |id| Frame::from_reference(&app.session, id))
}
fn run(app: &mut RustGisApp, id: &str, p: serde_json::Value) {
    if let Err(e) = app.run(id, p) {
        app.session.notify(e, true);
    }
}
pub(crate) fn panel(app: &mut RustGisApp, ctx: &egui::Context) {
    if app.session.tool != Tool::RotatedBox || app.session.active_layout.is_some() || app.mb.shown() || app.ws.catalog_view {
        return;
    }
    refresh_classes(app);
    egui::Window::new(tl!("Rotated Box Annotations"))
        .id(egui::Id::unique("box_annotation_controls"))
        .default_pos(Pos2::new((ctx.content_rect().right() - 310.).max(300.), 235.))
        .default_width(280.)
        .resizable(false)
        .vscroll(true)
        .default_height((ctx.content_rect().bottom() - 300.).max(240.))
        .min_height((ctx.content_rect().bottom() - 300.).max(240.))
        .max_height((ctx.content_rect().bottom() - 300.).max(240.))
        .show(ctx, |ui| {
            if let Some(l) = app.session.active_layer.and_then(|id| app.session.map().layer(id)) {
                ui.label(crate::i18n::fmt("Annotating: {layer}", &[("layer", &l.name)]));
            }
            ui.small(tl!("Each drag adds another box to this layer. Space + drag pans without leaving annotation mode."));
            if !ready(app) {
                ui.label(tl!("Create an annotation layer, or select an existing one in Layers."));
                if ui.button(tl!("New Box Annotation Layer")).clicked() {
                    run(app, "annotation.newLayer", json!({}));
                }
            }
            let refs: Vec<_> = app.session.map().layers.iter().filter(|l| l.is_raster()).map(|l| (l.id, l.name.clone())).collect();
            let title = app
                .boxes
                .reference
                .and_then(|id| refs.iter().find(|r| r.0 == id))
                .map(|r| r.1.clone())
                .unwrap_or_else(|| tl!("Web Mercator grid").into());
            let reference_label = ui.label(tl!("Reference for new boxes"));
            let before = app.boxes.reference;
            egui::ComboBox::from_id_salt("box_reference")
                .selected_text(title)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut app.boxes.reference, None, tl!("Web Mercator grid"));
                    for (id, name) in refs {
                        ui.selectable_value(&mut app.boxes.reference, Some(id), name);
                    }
                })
                .response
                .labelled_by(reference_label.id);
            if before != app.boxes.reference {
                app.boxes.points.clear();
                app.boxes.preview = None;
                app.boxes.gesture = None;
            }
            if let Err(e) = frame(app) {
                ui.colored_label(Color32::RED, e);
            }
            ui.small(tl!("Pixel coordinates use the loaded raster grid. Rotated source images are resampled by the reader."));
            if app.boxes.reference.is_none() {
                ui.small(tl!("No image reference: boxes use Web Mercator grid metres, not ground metres."));
            }
            let class_label = ui.label(tl!("Class for new boxes"));
            ui.add(egui::TextEdit::singleline(&mut app.boxes.class).char_limit(200).hint_text(tl!("e.g. vehicle")))
                .labelled_by(class_label.id);
            let colors = app.session.active_layer.map(|id| class_colors(&app.session, id)).unwrap_or_default();
            ui.horizontal(|ui| {
                if ui.add_enabled(ready(app), egui::Button::new(tl!("Add Class"))).clicked() {
                    run(app, "annotation.setClass", json!({"class":app.boxes.class}));
                }
                let c = colors.iter().find(|(name,_)| name == &app.boxes.class).map(|(_,c)| *c)
                    .unwrap_or_else(|| default_class_color(colors.len()));
                let mut rgb = [c.r(), c.g(), c.b()];
                let label = ui.label(tl!("Class colour"));
                let r = ui.add_enabled_ui(ready(app), |ui| ui.color_edit_button_srgb(&mut rgb)).inner.labelled_by(label.id);
                if r.changed() {
                    crate::widgets::one_step_per_press(app, &r);
                    run(app, "annotation.setClass", json!({"class":app.boxes.class,"color":rustgis_engine::map::Rgba::rgb(rgb[0],rgb[1],rgb[2]).hex()}));
                }
            });
            if app.boxes.classes.limited {
                ui.small(tl!("Class suggestions use the first 10,000 features. Type any other class above."));
            }
            if !app.boxes.classes.values.is_empty() || !colors.is_empty() {
                let existing_label = ui.label(tl!("Existing classes"));
                egui::ComboBox::from_id_salt("box_classes")
                    .selected_text(tl!("Reuse a class"))
                    .show_ui(ui, |ui| {
                        for (name, color) in &colors {
                            ui.horizontal(|ui| {
                                ui.colored_label(Color32::from_rgb(color.r(),color.g(),color.b()), "●");
                                let label = if name.is_empty() { tl!("Unclassified") } else { name.as_str() };
                                ui.selectable_value(&mut app.boxes.class, name.clone(), label);
                            });
                        }
                        for c in &app.boxes.classes.values {
                            if !colors.iter().any(|(name,_)| name == c) {
                                ui.selectable_value(&mut app.boxes.class, c.clone(), c.as_str());
                            }
                        }
                    })
                    .response
                    .labelled_by(existing_label.id);
            }
            let old = app.boxes.three_clicks;
            ui.checkbox(&mut app.boxes.three_clicks, tl!("Three-click creation"));
            if old != app.boxes.three_clicks {
                app.boxes.points.clear();
                app.boxes.preview = None;
                app.boxes.gesture = None;
            }
            ui.small(tl!(
                "Drag a box, then drag its paddle to rotate. Shift snaps rotation to 15°. Esc cancels. Space + drag or middle-drag pans; wheel zooms."
            ));
            ui.small(tl!("Hold Alt to draw a new box over an existing box."));
            if app.boxes.three_clicks {
                ui.small(tl!("Click the start and end of one edge, then set the perpendicular height."));
            }
            ui.separator();
            match selected(app) {
                Ok((layer, row, mut b)) => {
                    if let Some(l) = app.session.map().layer(layer) && let Some(fi) = l.dataset.field_index("class") {
                        let value = l.dataset.value(row, fi).to_string();
                        ui.label(crate::i18n::fmt("Selected class: {class}", &[("class", &value)]));
                    }
                    ui.label(tl!("Selected box"));
                    ui.small(if b.frame.image.is_some() {
                        tl!("Dimensions in reference pixels; angle clockwise from image right.")
                    } else if b.frame == Frame::default() {
                        tl!("Dimensions in Web Mercator grid metres, not ground metres; angle clockwise from east.")
                    } else {
                        tl!("Dimensions in stored local grid units; angle clockwise from its positive x axis.")
                    });
                    let mut changed = false;
                    egui::Grid::new("box_dimensions").show(ui, |ui| {
                        for (label, v) in [(tl!("Width"), &mut b.width), (tl!("Height"), &mut b.height), (tl!("Angle (°)"), &mut b.angle)]
                        {
                            let label = ui.label(label);
                            let r = ui.add(egui::DragValue::new(v).update_while_editing(false).speed(0.1)).labelled_by(label.id);
                            if crate::widgets::changed(&r) {
                                crate::widgets::one_step_per_press(app, &r);
                                changed = true;
                            }
                            ui.end_row();
                        }
                    });
                    if changed {
                        run(app, "annotation.updateBox", json!({"layer":layer,"row":row,"box":b}));
                    }
                    if ui.button(tl!("Apply Class to Selected Box")).clicked() {
                        run(app, "annotation.updateBox", json!({"layer":layer,"row":row,"box":b,"class":app.boxes.class}));
                    }
                }
                Err(e) => {
                    ui.small(e);
                }
            }
            if let Some(e) = &app.boxes.error {
                ui.colored_label(Color32::RED, e);
            }
            if ui.button(tl!("Pan / Stop Annotating")).clicked() {
                run(app, "tool.set", json!({"tool":"explore"}));
            }
            ui.small(tl!("Export this layer with Annotate › Export Features as GeoJSON."));
        });
}
fn coords(app: &RustGisApp, rect: Rect, p: Pos2, f: &Frame) -> Option<[f64; 2]> {
    f.to_local(screen_to_lonlat(app, rect, p)?)
}
fn screen(app: &RustGisApp, rect: Rect, b: &BoxSpec, xy: [f64; 2]) -> Option<Pos2> {
    lonlat_to_screen(app, rect, b.frame.to_lonlat(xy)?)
}
fn handles(app: &RustGisApp, rect: Rect, b: &BoxSpec) -> Vec<(Handle, Pos2)> {
    let mut out = Vec::new();
    if let (Some(c), Some(top)) = (screen(app, rect, b, b.center), screen(app, rect, b, b.point(0., -b.height / 2.))) {
        let d = top - c;
        let dir = if d.length() > 0.01 { d.normalized() } else { vec2(0., -1.) };
        out.push((Handle::Rotate, c + dir * (d.length() + 32.).max(64.)));
    }
    // Corners take priority when a small box's handles overlap.
    for (x, y) in [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.), (0., -1.), (1., 0.), (0., 1.), (-1., 0.)] {
        if let Some(p) = screen(app, rect, b, b.point(x * b.width / 2., y * b.height / 2.)) {
            out.push((Handle::Resize(x, y), p));
        }
    }
    out
}
fn hit(app: &RustGisApp, rect: Rect, p: Pos2, b: &BoxSpec) -> Option<Handle> {
    for (h, q) in handles(app, rect, b) {
        if p.distance(q) <= 9. {
            return Some(h);
        }
    }
    let q = coords(app, rect, p, &b.frame)?;
    let (u, v) = b.axes();
    let d = [q[0] - b.center[0], q[1] - b.center[1]];
    ((d[0] * u[0] + d[1] * u[1]).abs() <= b.width / 2. && (d[0] * v[0] + d[1] * v[1]).abs() <= b.height / 2.).then_some(Handle::Move)
}
fn initial(f: Frame, a: [f64; 2], p: [f64; 2]) -> BoxSpec {
    BoxSpec {
        version: 1,
        frame: f,
        center: [(a[0] + p[0]) / 2., (a[1] + p[1]) / 2.],
        width: (p[0] - a[0]).abs(),
        height: (p[1] - a[1]).abs(),
        angle: 0.,
    }
}
fn three(f: Frame, a: [f64; 2], b: [f64; 2], p: [f64; 2]) -> Option<BoxSpec> {
    let d = [b[0] - a[0], b[1] - a[1]];
    let w = d[0].hypot(d[1]);
    if w < 1e-6 {
        return None;
    }
    let v = [-d[1] / w, d[0] / w];
    let h = (p[0] - a[0]) * v[0] + (p[1] - a[1]) * v[1];
    Some(BoxSpec {
        version: 1,
        frame: f,
        center: [(a[0] + b[0]) / 2. + v[0] * h / 2., (a[1] + b[1]) / 2. + v[1] * h / 2.],
        width: w,
        height: h.abs(),
        angle: d[1].atan2(d[0]).to_degrees().rem_euclid(360.),
    })
}
fn manipulated(b: &BoxSpec, start: [f64; 2], p: [f64; 2], h: Handle, snap: bool) -> BoxSpec {
    let mut out = b.clone();
    match h {
        Handle::New => return initial(b.frame.clone(), start, p),
        Handle::Move => out.center = [b.center[0] + p[0] - start[0], b.center[1] + p[1] - start[1]],
        Handle::Rotate => {
            let a = (start[1] - b.center[1]).atan2(start[0] - b.center[0]);
            let c = (p[1] - b.center[1]).atan2(p[0] - b.center[0]);
            out.angle = (b.angle + (c - a).to_degrees()).rem_euclid(360.);
            if snap {
                out.angle = (out.angle / 15.).round() * 15.;
            }
        }
        Handle::Resize(x, y) => {
            let anchor = b.point(-x * b.width / 2., -y * b.height / 2.);
            let (u, v) = b.axes();
            // A handle has a generous hit target: preserve the grab offset so pressing near
            // its centre never makes the box jump before the pointer actually moves.
            let handle = b.point(x * b.width / 2., y * b.height / 2.);
            let d = [handle[0] + p[0] - start[0] - anchor[0], handle[1] + p[1] - start[1] - anchor[1]];
            let dx = if x == 0. { 0. } else { d[0] * u[0] + d[1] * u[1] };
            let dy = if y == 0. { 0. } else { d[0] * v[0] + d[1] * v[1] };
            out.center = [anchor[0] + (u[0] * dx + v[0] * dy) / 2., anchor[1] + (u[1] * dx + v[1] * dy) / 2.];
            if x != 0. {
                out.width = dx.abs();
            }
            if y != 0. {
                out.height = dy.abs();
            }
        }
    }
    out
}
/// A Space-started drag remains navigation until release, even if Space is released first.
pub(crate) fn pan_gesture(app: &mut RustGisApp, ui: &Ui, resp: &egui::Response) -> bool {
    let space = ui.input(|i| i.key_down(egui::Key::Space)) && !ui.ctx().egui_wants_keyboard_input();
    if (resp.drag_started_by(egui::PointerButton::Primary) || resp.dragged_by(egui::PointerButton::Primary)) && space {
        app.boxes.panning = true;
        app.boxes.gesture = None;
        if app.boxes.points.is_empty() {
            app.boxes.preview = None;
        }
    }
    if resp.dragged_by(egui::PointerButton::Middle) {
        app.boxes.gesture = None;
        if app.boxes.points.is_empty() {
            app.boxes.preview = None;
        }
    }
    let pan = app.boxes.panning || space;
    if !ui.input(|i| i.pointer.primary_down()) {
        app.boxes.panning = false;
    }
    pan
}
pub(crate) fn interact(app: &mut RustGisApp, ui: &Ui, resp: &egui::Response, rect: Rect) {
    follow(app);
    if !supported(app) {
        app.boxes.gesture = None;
        app.boxes.preview = None;
        app.boxes.points.clear();
        app.boxes.error = Some(tl!("Box annotation requires a flat, untilted Earth map.").into());
        return;
    }
    if !ui.ctx().egui_wants_keyboard_input()
        && ui.ctx().memory(|m| m.top_modal_layer()).is_none()
        && !app.ui.shortcut_editor.capturing()
        && ui.input(|i| i.key_pressed(egui::Key::Escape))
    {
        run(app, "tool.set", json!({"tool":"explore"}));
        return;
    }
    if !app.session.map().editing.settings.enabled {
        return;
    }
    // Navigation does not move any annotation.
    if resp.dragged_by(egui::PointerButton::Middle) {
        return;
    }
    let id = match app.session.active_layer {
        Some(id) => id,
        None => return,
    };
    let Some(l) = app.session.map().layer(id) else { return };
    if !ready(app) {
        return;
    }
    let hover = resp.hover_pos();
    if let (Some(p), Ok((_, _, b))) = (hover, selected(app))
        && let Some(h) = hit(app, rect, p, &b)
    {
        ui.ctx().set_cursor_icon(match h {
            Handle::Rotate => egui::CursorIcon::Grab,
            Handle::Move => egui::CursorIcon::Move,
            _ => egui::CursorIcon::Crosshair,
        });
    }
    if resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(origin) = ui.input(|i| i.pointer.press_origin())
    {
        let chosen = (!ui.input(|i| i.modifiers.alt))
            .then(|| selected(app).ok())
            .flatten()
            .and_then(|(_, row, b)| hit(app, rect, origin, &b).map(|h| (Some(row), b, h)));
        let chosen = chosen.or_else(|| {
            if app.boxes.three_clicks {
                return None;
            }
            let f = frame(app).ok()?;
            let a = coords(app, rect, origin, &f)?;
            Some((None, initial(f, a, a), Handle::New))
        });
        if let Some((row, b, h)) = chosen
            && let Some(start) = coords(app, rect, origin, &b.frame)
        {
            let rows = l.dataset.features.allocation_key();
            app.boxes.gesture = Some(Gesture { layer: id, row, rows, original: b, start, handle: h });
            app.boxes.error = None;
        }
    }
    if let Some(g) = &app.boxes.gesture
        && let Some(p) = ui.input(|i| i.pointer.latest_pos())
    {
        if let Some(q) = coords(app, rect, p, &g.original.frame) {
            let b = manipulated(&g.original, g.start, q, g.handle, ui.input(|i| i.modifiers.shift));
            app.boxes.error = b.geometry().err();
            app.boxes.preview = Some(b);
        } else {
            app.boxes.error = Some(tl!("Pointer is outside the reference coordinate system.").into());
            app.boxes.preview = None;
        }
    }
    if resp.drag_stopped_by(egui::PointerButton::Primary)
        && let Some(g) = app.boxes.gesture.take()
        && let Some(b) = app.boxes.preview.take()
        && app.boxes.error.is_none()
    {
        if let Some(row) = g.row {
            if b != g.original {
                run(app, "annotation.updateBox", json!({"layer":id,"row":row,"box":b}));
            }
        } else {
            run(app, "annotation.addBox", json!({"layer":id,"box":b,"class":app.boxes.class}));
        }
    }
    if app.boxes.gesture.is_none()
        && app.boxes.three_clicks
        && !app.boxes.points.is_empty()
        && let Some(p) = hover
        && let Some(f) = app.boxes.sketch_frame.clone()
        && let Some(q) = coords(app, rect, p, &f)
    {
        app.boxes.preview = match app.boxes.points.as_slice() {
            [a] => three(f, *a, q, q),
            [a, b] => three(f, *a, *b, q),
            _ => None,
        };
        app.boxes.error = (app.boxes.points.len() == 2).then(|| app.boxes.preview.as_ref().and_then(|b| b.geometry().err())).flatten();
    }
    if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        // Click on an existing box selects it; clicking empty space begins a three-point box.
        if app.boxes.points.is_empty()
            && !ui.input(|i| i.modifiers.alt)
            && let Some(c) = screen_to_lonlat(app, rect, p)
        {
            let scale = app.session.scale();
            let tolerance = 4. * app.session.map().camera.res;
            let hit =
                app.session.map().to_map(c).and_then(|p| app.session.map_mut().hit(p, tolerance, scale, |l| l.id == id && l.selectable));
            let rows: Vec<_> = hit.map(|(_, row)| row).into_iter().collect();
            run(app, "select.rows", json!({"layer":id,"rows":rows}));
            if selected(app).is_ok() {
                return;
            }
        }
        if app.boxes.three_clicks {
            match frame(app) {
                Ok(f) => {
                    if let Some(q) = coords(app, rect, p, &f) {
                        if app.boxes.points.len() < 2 {
                            if app.boxes.points.first().is_some_and(|a| (a[0] - q[0]).hypot(a[1] - q[1]) < 1e-6) {
                                return;
                            }
                            app.boxes.sketch_frame = Some(f);
                            app.boxes.points.push(q);
                        } else if let (Some(a), Some(b)) = (app.boxes.points.first(), app.boxes.points.get(1))
                            && let Some(b) = three(f, *a, *b, q)
                        {
                            match b.geometry() {
                                Ok(_) => match app.run("annotation.addBox", json!({"layer":id,"box":b,"class":app.boxes.class})) {
                                    Ok(_) => cancel(&mut app.boxes),
                                    Err(e) => app.boxes.error = Some(e),
                                },
                                Err(e) => app.boxes.error = Some(e),
                            }
                        }
                    }
                }
                Err(e) => app.boxes.error = Some(e),
            }
        }
    }
}
pub(crate) fn draw(app: &RustGisApp, p: &egui::Painter, rect: Rect) {
    if app.session.tool != Tool::RotatedBox {
        return;
    }
    let selected = selected(app).ok();
    let Some(b) = app.boxes.preview.as_ref().or_else(|| selected.as_ref().map(|s| &s.2)) else { return };
    let corners: Vec<_> = b.corners().iter().filter_map(|xy| screen(app, rect, b, *xy)).collect();
    if corners.len() != 4 {
        return;
    }
    let class = if app.boxes.preview.is_some() && app.boxes.gesture.as_ref().is_none_or(|g| g.row.is_none()) {
        app.boxes.class.clone()
    } else {
        selected
            .as_ref()
            .and_then(|(id, row, _)| {
                app.session.map().layer(*id).and_then(|l| l.dataset.field_index("class").map(|fi| l.dataset.value(*row, fi).to_string()))
            })
            .unwrap_or_default()
    };
    let colors = app.session.active_layer.map(|id| class_colors(&app.session, id)).unwrap_or_default();
    let c = colors.iter().find(|(name, _)| name == &class).map(|(_, c)| *c).unwrap_or_else(|| default_class_color(colors.len()));
    let color = if app.boxes.error.is_some() { Color32::RED } else { Color32::from_rgb(c.r(), c.g(), c.b()) };
    let mut outline = corners.clone();
    if let Some(first) = outline.first().copied() {
        outline.push(first);
    }
    p.add(egui::Shape::line(outline, Stroke::new(2., color)));
    if !app.boxes.points.is_empty() {
        return;
    }
    for (h, q) in handles(app, rect, b) {
        match h {
            Handle::Rotate => {
                if let Some(top) = screen(app, rect, b, b.point(0., -b.height / 2.)) {
                    p.line_segment([top, q], Stroke::new(1.5, color));
                }
                p.circle_filled(q, 7., Color32::WHITE);
                p.circle_stroke(q, 7., Stroke::new(2., color));
                p.text(
                    q + vec2(13., 0.),
                    egui::Align2::LEFT_CENTER,
                    format!("{:.1}°", b.angle.rem_euclid(360.)),
                    egui::FontId::proportional(12.),
                    color,
                );
            }
            _ => {
                let r = Rect::from_center_size(q, vec2(8., 8.));
                p.rect_filled(r, 1., Color32::WHITE);
                p.rect_stroke(r, 1., Stroke::new(1.5, color), egui::StrokeKind::Middle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn harness() -> egui_kittest::Harness<'static, RustGisApp> {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("annotation.newLayer", json!({})).unwrap();
        app.run("map.zoomToExtent", json!({"extent":[-0.005,-0.005,0.005,0.005]})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400., 860.)).with_step_dt(0.016).with_max_steps(10000).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.step();
        h
    }
    fn pointer(h: &mut egui_kittest::Harness<'_, RustGisApp>, p: Pos2, pressed: Option<bool>) {
        h.event(egui::Event::PointerMoved(p));
        if let Some(pressed) = pressed {
            h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        }
        h.step();
    }
    #[test]
    fn annotation_real_ui_creates_rotates_undoes_and_cancels() {
        let mut h = harness();
        let rect = h.state().map.view_rect.unwrap().0;
        let a = rect.center() - vec2(80., 45.);
        let z = rect.center() + vec2(80., 45.);
        pointer(&mut h, a, None);
        pointer(&mut h, a, Some(true));
        pointer(&mut h, z, None);
        pointer(&mut h, z, Some(false));
        let (_, _, before) = selected(h.state()).expect("drag creates a selected box");
        assert!(before.width > 0. && before.height > 0.);
        let paddle = handles(h.state(), rect, &before).into_iter().find(|(h, _)| matches!(h, Handle::Rotate)).unwrap().1;
        let centre = screen(h.state(), rect, &before, before.center).unwrap();
        let d = paddle - centre;
        let end = centre + vec2(d.x * 0.8660254 - d.y * 0.5, d.x * 0.5 + d.y * 0.8660254);
        pointer(&mut h, paddle, None);
        pointer(&mut h, paddle, Some(true));
        pointer(&mut h, end, None);
        pointer(&mut h, end, Some(false));
        let rotated = selected(h.state()).unwrap().2;
        assert!((rotated.angle - 30.).abs() < 0.2, "paddle angle: {}", rotated.angle);
        near(rotated.center, before.center);
        assert_eq!(rotated.width, before.width);
        h.state_mut().run("edit.undo", json!({})).unwrap();
        h.step();
        assert_eq!(selected(h.state()).unwrap().2, before);
        // Cancel a move; the next release must not commit the discarded preview.
        pointer(&mut h, centre, None);
        pointer(&mut h, centre, Some(true));
        pointer(&mut h, centre + vec2(45., 10.), None);
        h.state_mut().ui.keymap.bindings.retain(|b| b.chord.key != egui::Key::Escape);
        h.event(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        pointer(&mut h, centre + vec2(45., 10.), Some(false));
        assert_eq!(selected(h.state()).unwrap().2, before);
        assert_eq!(h.state().session.tool, Tool::Explore);
        // Context/source edits during a held drag invalidate the release as well.
        h.state_mut().run("tool.set", json!({"tool":"rotatedBox"})).unwrap();
        h.step();
        pointer(&mut h, centre, None);
        pointer(&mut h, centre, Some(true));
        pointer(&mut h, centre + vec2(30., 15.), None);
        assert!(h.state().boxes.gesture.is_some(), "source edit interrupts a real held gesture");
        h.state_mut().run("edit.setValue", json!({"row":0,"field":"class","value":"changed"})).unwrap();
        h.step();
        pointer(&mut h, centre + vec2(30., 15.), Some(false));
        assert_eq!(selected(h.state()).unwrap().2, before);
    }

    fn click(h: &mut egui_kittest::Harness<'_, RustGisApp>, p: Pos2) {
        pointer(h, p, None);
        pointer(h, p, Some(true));
        pointer(h, p, Some(false));
    }
    #[test]
    fn annotation_three_click_failure_retains_draft_and_recovers() {
        let mut h = harness();
        h.state_mut().boxes.three_clicks = true;
        h.state_mut().run("data.domain", json!({"field":"class","coded":[["vehicle","Vehicle"]]})).unwrap();
        h.state_mut().boxes.class = "invalid".into();
        let rect = h.state().map.view_rect.unwrap().0;
        let a = rect.center();
        let b = a + vec2(110., 0.);
        let c = b + vec2(0., 70.);
        click(&mut h, a);
        click(&mut h, b);
        click(&mut h, b); // zero height
        assert!(h.state().boxes.error.is_some());
        pointer(&mut h, c, None);
        assert!(h.state().boxes.error.is_none(), "a valid preview clears the geometry error");
        click(&mut h, c); // invalid class fails the command
        assert_eq!(h.state().boxes.points.len(), 2);
        assert!(h.state().boxes.preview.is_some());
        assert_eq!(h.state().session.map().layers[0].dataset.features.len(), 0);
        h.state_mut().boxes.class = "vehicle".into();
        click(&mut h, c);
        assert!(h.state().boxes.points.is_empty());
        assert!(h.state().boxes.error.is_none());
        assert!(selected(h.state()).unwrap().2.height > 0.);
    }
    #[test]
    fn annotation_visibility_context_and_reference_cancel_held_edits() {
        let mut h = harness();
        h.state_mut().run("annotation.addBox", json!({"box":b()})).unwrap();
        h.step();
        let id = h.state().session.active_layer.unwrap();
        let rect = h.state().map.view_rect.unwrap().0;
        let before = selected(h.state()).unwrap().2;
        let centre = screen(h.state(), rect, &before, before.center).unwrap();
        for context in 0..5 {
            pointer(&mut h, centre, None);
            pointer(&mut h, centre, Some(true));
            pointer(&mut h, centre + vec2(35., 20.), None);
            assert!(h.state().boxes.gesture.is_some());
            match context {
                0 => h.state_mut().session.map_mut().layer_mut(id).unwrap().definition_query = Some("class = 'hidden'".into()),
                1 => {
                    let mut group = rustgis_engine::map::LayerGroup::new("Hidden");
                    group.children.push(id);
                    group.visible = false;
                    h.state_mut().session.map_mut().add_group(group);
                }
                2 => h.state_mut().ui.backstage = true,
                3 => h.state_mut().session.map_mut().camera.pitch = 20.,
                _ => h.state_mut().session.map_mut().editing.settings.enabled = false,
            }
            h.step();
            assert!(h.state().boxes.gesture.is_none());
            if context != 2 {
                assert!(selected(h.state()).is_err());
            }
            pointer(&mut h, centre + vec2(35., 20.), Some(false));
            let app = h.state_mut();
            app.session.map_mut().layer_mut(id).unwrap().definition_query = None;
            app.session.map_mut().groups.clear();
            app.ui.backstage = false;
            app.session.map_mut().camera.pitch = 0.;
            app.session.map_mut().editing.settings.enabled = true;
            h.step();
            assert_eq!(selected(h.state()).unwrap().2, before);
        }
        // Only a NEW drag follows the chosen reference; existing-box moves retain their frame.
        let app = h.state_mut();
        let rows = app.session.map().layer(id).unwrap().dataset.features.allocation_key();
        app.boxes.gesture =
            Some(Gesture { layer: id, row: None, rows, original: before.clone(), start: before.center, handle: Handle::New });
        app.boxes.reference = Some(u32::MAX); // removed/replaced reference
        follow(app);
        assert!(app.boxes.gesture.is_none());
        app.boxes.reference = None;
    }
    #[test]
    fn annotation_selection_ignores_overlapping_layers_and_point_picks_take_priority() {
        let mut h = harness();
        h.state_mut().run("annotation.addBox", json!({"box":b()})).unwrap();
        let id = h.state().session.active_layer.unwrap();
        h.state_mut().run("annotation.newLayer", json!({"name":"Above"})).unwrap();
        h.state_mut().run("annotation.addBox", json!({"box":b()})).unwrap();
        h.state_mut().session.active_layer = Some(id);
        h.state_mut().run("select.clear", json!({})).unwrap();
        h.step();
        let rect = h.state().map.view_rect.unwrap().0;
        let centre = screen(h.state(), rect, &b(), b().center).unwrap();
        click(&mut h, centre);
        assert_eq!(selected(h.state()).unwrap().0, id);
        h.state_mut().workflow.pick = Some(crate::analysispane::Pick { purpose: "test", needed: 1, points: vec![] });
        click(&mut h, centre + vec2(60., 30.));
        assert_eq!(h.state().workflow.pick.as_ref().unwrap().points.len(), 1);
        assert_eq!(h.state().session.map().layer(id).unwrap().dataset.features.len(), 1);
        assert!(h.state().boxes.points.is_empty());
        // Switching from an unfinished polygon must clear that tool's map sketch too.
        h.state_mut().run("tool.set", json!({"tool":"createPolygon"})).unwrap();
        click(&mut h, centre);
        click(&mut h, centre + vec2(80., 30.));
        assert!(h.state().map.sketch_len() > 0);
        h.state_mut().run("annotation.newLayer", json!({})).unwrap();
        assert_eq!(h.state().map.sketch_len(), 0);
    }
    #[test]
    fn annotation_schema_and_class_cache_follow_source_changes() {
        let mut h = harness();
        let app = h.state_mut();
        let id = app.session.active_layer.unwrap();
        let ds = &mut app.session.map_mut().layer_mut(id).unwrap().dataset;
        for _ in 0..10_001 {
            ds.push(
                None,
                vec![rustgis_engine::data::Value::Null, rustgis_engine::data::Value::Text("car".into()), rustgis_engine::data::Value::Null],
            );
        }
        refresh_classes(app);
        assert!(app.boxes.classes.limited);
        assert!(app.boxes.classes.values.contains("car"));
        let old = app.boxes.classes.rows.clone();
        refresh_classes(app);
        assert!(app.boxes.classes.rows.ptr_eq(&old));
        app.session.map_mut().layer_mut(id).unwrap().dataset.features.get_mut(0).unwrap().attrs[1] =
            rustgis_engine::data::Value::Text("truck".into());
        refresh_classes(app);
        assert!(!app.boxes.classes.rows.ptr_eq(&old));
        assert!(app.boxes.classes.values.contains("truck"));
        app.session.map_mut().layer_mut(id).unwrap().dataset.fields[0].ty = rustgis_engine::data::FieldType::Integer;
        assert!(!ready(app), "record field alone does not make an annotation schema");
    }
    #[test]
    fn annotation_space_pan_preserves_tool_and_more_boxes_use_same_layer() {
        let mut h = harness();
        let id = h.state().session.active_layer.unwrap();
        let rect = h.state().map.view_rect.unwrap().0;
        let a = rect.center();
        click(&mut h, a); // selection only, no box
        let camera = h.state().session.map().camera;
        h.event(egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        pointer(&mut h, a, Some(true));
        pointer(&mut h, a + vec2(80., 40.), None);
        h.event(egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        pointer(&mut h, a + vec2(100., 50.), None); // keeps panning after Space is released
        pointer(&mut h, a + vec2(100., 50.), Some(false));
        assert_ne!(h.state().session.map().camera, camera);
        assert_eq!(h.state().session.tool, Tool::RotatedBox);
        assert!(h.state().session.map().layer(id).unwrap().dataset.features.is_empty());
        for (offset, name) in [(0., "vehicle"), (190., "building")] {
            h.state_mut().boxes.class = name.into();
            let a = a + vec2(offset - 100., -90.);
            pointer(&mut h, a, Some(true));
            pointer(&mut h, a + vec2(100., 60.), None);
            pointer(&mut h, a + vec2(100., 60.), Some(false));
        }
        let l = h.state().session.map().layer(id).unwrap();
        assert_eq!(h.state().session.map().layers.len(), 1);
        assert_eq!(l.dataset.features.len(), 2);
        assert_ne!(l.renderer.symbol_for(&l.dataset, 0).stroke, l.renderer.symbol_for(&l.dataset, 1).stroke);
    }
    fn b() -> BoxSpec {
        BoxSpec { version: 1, frame: Frame::default(), center: [100., 200.], width: 80., height: 20., angle: 37. }
    }
    fn near(a: [f64; 2], b: [f64; 2]) {
        assert!((a[0] - b[0]).hypot(a[1] - b[1]) < 1e-8, "{a:?} != {b:?}");
    }
    #[test]
    fn annotation_resize_keeps_opposite_anchor_including_crossovers() {
        let b = b();
        for (x, y) in [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.), (0., -1.), (1., 0.), (0., 1.), (-1., 0.)] {
            for sign in [-1., 1.] {
                let old = b.point(x * b.width / 2., y * b.height / 2.);
                let anchor = b.point(-x * b.width / 2., -y * b.height / 2.);
                let pointer = b.point(x * (b.width / 2. + 40.) * sign, y * (b.height / 2. + 30.) * sign);
                let out = manipulated(&b, old, pointer, Handle::Resize(x, y), false);
                assert!(out.width > 0. && out.height > 0.);
                assert_eq!(out.angle, b.angle);
                let corners = out.corners();
                if x != 0. && y != 0. {
                    assert!(corners.iter().any(|c| (c[0] - anchor[0]).hypot(c[1] - anchor[1]) < 1e-8));
                } else if x == 0. {
                    assert_eq!(out.width, b.width);
                } else {
                    assert_eq!(out.height, b.height);
                }
            }
        }
    }
    #[test]
    fn annotation_rotation_keeps_centre_and_size_and_snaps() {
        let b = b();
        let start = b.point(0., -100.);
        let out = manipulated(&b, start, b.point(100., 0.), Handle::Rotate, false);
        near(out.center, b.center);
        assert_eq!(out.width, b.width);
        assert_eq!(out.height, b.height);
        assert!((out.angle - 127.).abs() < 1e-8);
        let snapped = manipulated(&b, start, b.point(100., 0.), Handle::Rotate, true);
        assert_eq!(snapped.angle, 120.);
        let out = manipulated(&b, start, start, Handle::Rotate, false);
        assert!((out.angle - b.angle).abs() < 1e-8);
    }
    #[test]
    fn annotation_three_clicks_preserve_the_first_edge_on_either_side() {
        let a = [100., 200.];
        let c = [160., 280.];
        for p in [[180., 200.], [0., 300.]] {
            let out = three(Frame::default(), a, c, p).unwrap();
            let corners = out.corners();
            assert!((out.width - 100.).abs() < 1e-8);
            assert!(corners.iter().any(|q| (q[0] - a[0]).hypot(q[1] - a[1]) < 1e-8));
            assert!(corners.iter().any(|q| (q[0] - c[0]).hypot(q[1] - c[1]) < 1e-8));
        }
        assert!(three(Frame::default(), a, a, c).is_none());
    }
}
