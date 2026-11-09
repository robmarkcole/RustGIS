//! Point placement, circle radii and simple polygon vertices in the shared annotation workflow.
use crate::{
    RustGisApp, boxedit,
    mapview::{lonlat_to_screen, screen_to_lonlat},
};
use egui::{Color32, Pos2, Rect, Stroke, Ui, vec2};
use rustgis_engine::{
    Tool,
    cmd::annotation::{Frame, MAX_VERTICES, Shape, ShapeSpec, class_colors, default_class_color},
    data::{Feature, Geometry},
};
use serde_json::json;

#[derive(Default)]
pub(crate) struct State {
    vertices: Vec<[f64; 2]>,
    frame: Option<Frame>,
    drag: Option<Drag>,
    preview: Option<ShapeSpec>,
}
struct Drag {
    layer: u32,
    row: Option<usize>,
    rows: std::sync::Weak<Vec<Feature>>,
    original: ShapeSpec,
    start: [f64; 2],
    handle: Handle,
}
#[derive(Clone, Copy)]
enum Handle {
    Move,
    Radius,
    Vertex(usize),
    NewCircle,
}
pub(crate) fn cancel_drag(app: &mut RustGisApp) {
    app.boxes.other.drag = None;
    app.boxes.other.preview = None;
}
pub(crate) fn cancel(app: &mut RustGisApp) {
    app.boxes.other = State::default();
}
pub(crate) fn follow(app: &mut RustGisApp) {
    let state = &app.boxes.other;
    let valid = state.drag.as_ref().is_none_or(|g| {
        app.session.map().layer(g.layer).is_some_and(|l| {
            g.rows.ptr_eq(&l.dataset.features.allocation_key())
                && app.session.map().draws_at(l, app.session.scale())
                && g.row.is_none_or(|r| l.selection == [r] && l.drawn_rows(app.session.scale()).get(r) == Some(&true))
        }) && (g.row.is_some() || boxedit::frame(app).as_ref() == Ok(&g.original.frame))
    }) && state.frame.as_ref().is_none_or(|f| boxedit::frame(app).as_ref() == Ok(f));
    if !valid {
        cancel(app);
    }
}
fn selected(app: &RustGisApp) -> Result<(u32, usize, ShapeSpec), String> {
    let (id, row) = boxedit::selected_row(app)?;
    let l = app.session.map().layer(id).ok_or(tl!("no layer"))?;
    let shape = ShapeSpec::from_row(&l.dataset, row)?;
    if shape.tool() != app.session.tool {
        return Err(tl!("Choose the selected annotation's shape tool to edit it.").into());
    }
    Ok((id, row, shape))
}
fn local(app: &RustGisApp, rect: Rect, p: Pos2, f: &Frame) -> Option<[f64; 2]> {
    f.to_local(screen_to_lonlat(app, rect, p)?)
}
fn screen(app: &RustGisApp, rect: Rect, f: &Frame, p: [f64; 2]) -> Option<Pos2> {
    lonlat_to_screen(app, rect, f.to_lonlat(p)?)
}
fn handles(shape: &ShapeSpec) -> Vec<(Handle, [f64; 2])> {
    match &shape.shape {
        Shape::Point { position } => vec![(Handle::Move, *position)],
        Shape::Circle { center, radius } => vec![
            (Handle::Move, *center),
            (Handle::Radius, [center[0] + radius, center[1]]),
            (Handle::Radius, [center[0] - radius, center[1]]),
            (Handle::Radius, [center[0], center[1] + radius]),
            (Handle::Radius, [center[0], center[1] - radius]),
        ],
        Shape::Polygon { vertices } => vertices.iter().enumerate().map(|(i, p)| (Handle::Vertex(i), *p)).collect(),
    }
}
fn hit(app: &RustGisApp, rect: Rect, p: Pos2, shape: &ShapeSpec) -> Option<Handle> {
    if let Some((handle, _)) = handles(shape)
        .into_iter()
        .filter_map(|(h, q)| screen(app, rect, &shape.frame, q).map(|q| (h, q.distance(p))))
        .filter(|(_, distance)| *distance <= 9.)
        .min_by(|a, b| a.1.total_cmp(&b.1))
    {
        return Some(handle);
    }
    let q = local(app, rect, p, &shape.frame)?;
    match &shape.shape {
        Shape::Circle { center, radius } => ((q[0] - center[0]).hypot(q[1] - center[1]) <= *radius).then_some(Handle::Move),
        Shape::Polygon { vertices } => {
            let mut inside = false;
            for i in 0..vertices.len() {
                let (Some(a), Some(b)) = (vertices.get(i), vertices.get((i + 1) % vertices.len())) else { return None };
                if (a[1] > q[1]) != (b[1] > q[1]) && q[0] < (b[0] - a[0]) * (q[1] - a[1]) / (b[1] - a[1]) + a[0] {
                    inside = !inside;
                }
            }
            inside.then_some(Handle::Move)
        }
        _ => None,
    }
}
fn manipulated(original: &ShapeSpec, start: [f64; 2], p: [f64; 2], h: Handle) -> ShapeSpec {
    let mut out = original.clone();
    let dx = p[0] - start[0];
    let dy = p[1] - start[1];
    match (&mut out.shape, h) {
        (Shape::Circle { radius, center }, Handle::Radius) => {
            *radius = (*radius + (p[0] - center[0]).hypot(p[1] - center[1]) - (start[0] - center[0]).hypot(start[1] - center[1])).abs();
        }
        (Shape::Circle { radius, center }, Handle::NewCircle) => *radius = (p[0] - center[0]).hypot(p[1] - center[1]),
        (Shape::Polygon { vertices }, Handle::Vertex(i)) => {
            if let Some(v) = vertices.get_mut(i) {
                v[0] += dx;
                v[1] += dy;
            }
        }
        (Shape::Polygon { vertices }, Handle::Move) => {
            for v in vertices {
                v[0] += dx;
                v[1] += dy;
            }
        }
        (Shape::Point { position }, Handle::Move) => {
            position[0] += dx;
            position[1] += dy;
        }
        (Shape::Circle { center, .. }, Handle::Move) => {
            center[0] += dx;
            center[1] += dy;
        }
        _ => {}
    }
    out
}
fn save(app: &mut RustGisApp, id: u32, row: Option<usize>, shape: &ShapeSpec) -> bool {
    let result = if let Some(row) = row {
        app.run("annotation.updateShape", json!({"layer":id,"row":row,"shape":shape}))
    } else {
        app.run("annotation.addShape", json!({"layer":id,"shape":shape,"class":app.boxes.class}))
    };
    match result {
        Ok(_) => {
            app.boxes.error = None;
            true
        }
        Err(e) => {
            app.boxes.error = Some(e);
            false
        }
    }
}
fn finish_polygon(app: &mut RustGisApp, id: u32) {
    let Some(frame) = app.boxes.other.frame.clone() else { return };
    let shape = ShapeSpec { version: 1, frame, shape: Shape::Polygon { vertices: app.boxes.other.vertices.clone() } };
    match shape.geometry() {
        Ok(_) => {
            if save(app, id, None, &shape) {
                cancel(app);
            }
        }
        Err(e) => app.boxes.error = Some(e),
    }
}
pub(crate) fn interact(app: &mut RustGisApp, ui: &Ui, resp: &egui::Response, rect: Rect) {
    let Some(id) = app.session.active_layer.filter(|_| boxedit::ready(app)) else { return };
    if !ui.ctx().egui_wants_keyboard_input() && ui.ctx().memory(|m| m.top_modal_layer()).is_none() {
        if ui.input(|i| i.key_pressed(egui::Key::Backspace)) {
            app.boxes.other.vertices.pop();
            app.boxes.error = None;
        }
        if app.session.tool == Tool::AnnotationPolygon && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            finish_polygon(app, id);
            return;
        }
    }
    if resp.drag_started_by(egui::PointerButton::Primary)
        && app.boxes.other.vertices.is_empty()
        && let Some(origin) = ui.input(|i| i.pointer.press_origin())
    {
        let existing = (!ui.input(|i| i.modifiers.alt))
            .then(|| selected(app).ok())
            .flatten()
            .and_then(|(_, row, s)| hit(app, rect, origin, &s).map(|h| (Some(row), s, h)));
        let chosen = existing.or_else(|| {
            if app.session.tool != Tool::AnnotationCircle {
                return None;
            }
            let f = boxedit::frame(app).ok()?;
            let p = local(app, rect, origin, &f)?;
            Some((None, ShapeSpec { version: 1, frame: f, shape: Shape::Circle { center: p, radius: 0. } }, Handle::NewCircle))
        });
        if let Some((row, s, handle)) = chosen
            && let Some(start) = local(app, rect, origin, &s.frame)
            && let Some(l) = app.session.map().layer(id)
        {
            app.boxes.other.drag = Some(Drag { layer: id, row, rows: l.dataset.features.allocation_key(), original: s, start, handle });
            app.boxes.error = None;
        }
    }
    if let Some(g) = &app.boxes.other.drag
        && let Some(p) = ui.input(|i| i.pointer.latest_pos())
    {
        if let Some(q) = local(app, rect, p, &g.original.frame) {
            let s = manipulated(&g.original, g.start, q, g.handle);
            app.boxes.error = s.geometry().err();
            app.boxes.other.preview = Some(s);
        } else {
            app.boxes.error = Some(tl!("Pointer is outside the reference coordinate system.").into());
            app.boxes.other.preview = None;
        }
    }
    if resp.drag_stopped_by(egui::PointerButton::Primary)
        && let Some(g) = app.boxes.other.drag.take()
        && let Some(s) = app.boxes.other.preview.take()
        && app.boxes.error.is_none()
        && (g.row.is_none() || s != g.original)
    {
        save(app, id, g.row, &s);
    }
    if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        if app.boxes.other.vertices.is_empty()
            && !ui.input(|i| i.modifiers.alt)
            && let Some(c) = screen_to_lonlat(app, rect, p)
        {
            let scale = app.session.scale();
            let tolerance = 5. * app.session.map().camera.res;
            let hit =
                app.session.map().to_map(c).and_then(|p| app.session.map_mut().hit(p, tolerance, scale, |l| l.id == id && l.selectable));
            if let Some((_, row)) = hit {
                let _ = app.run("select.rows", json!({"layer":id,"rows":[row]}));
                if let Some(l) = app.session.map().layer(id) {
                    if let Ok(s) = ShapeSpec::from_row(&l.dataset, row) {
                        let _ = app.run("tool.set", json!({"tool":s.tool().code()}));
                        return;
                    }
                    if rustgis_engine::cmd::annotation::BoxSpec::from_row(&l.dataset, row).is_ok() {
                        let _ = app.run("tool.set", json!({"tool":"rotatedBox"}));
                        return;
                    }
                }
                app.boxes.error=Some(tl!("This feature has an unsupported annotation record. Use ordinary feature editing, or hold Alt to create an overlapping annotation.").into());
                return;
            }
        }
        let f = match app.boxes.other.frame.clone().map(Ok).unwrap_or_else(|| boxedit::frame(app)) {
            Ok(f) => f,
            Err(e) => {
                app.boxes.error = Some(e);
                return;
            }
        };
        let Some(q) = local(app, rect, p, &f) else {
            app.boxes.error = Some(tl!("Pointer is outside the reference coordinate system.").into());
            return;
        };
        match app.session.tool {
            Tool::AnnotationPoint => {
                save(app, id, None, &ShapeSpec { version: 1, frame: f, shape: Shape::Point { position: q } });
            }
            Tool::AnnotationPolygon => {
                let near_start = app.boxes.other.vertices.len() >= 3
                    && app.boxes.other.vertices.first().and_then(|v| screen(app, rect, &f, *v)).is_some_and(|v| v.distance(p) <= 9.);
                if near_start || resp.double_clicked() {
                    finish_polygon(app, id);
                    return;
                }
                if app.boxes.other.vertices.len() >= MAX_VERTICES {
                    app.boxes.error = Some(tl!("Polygon vertex limit reached; finish or cancel the outline.").into());
                    return;
                }
                if app.boxes.other.vertices.last().is_none_or(|v| (v[0] - q[0]).hypot(v[1] - q[1]) > 1e-6) {
                    app.boxes.other.frame = Some(f);
                    app.boxes.other.vertices.push(q);
                    app.boxes.error = None;
                }
            }
            _ => {}
        }
    }
    if resp.hovered() && selected(app).ok().zip(resp.hover_pos()).is_some_and(|((_, _, s), p)| hit(app, rect, p, &s).is_some()) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
    }
}
pub(crate) fn panel_selected(app: &mut RustGisApp, ui: &mut Ui) {
    if !app.boxes.other.vertices.is_empty() {
        ui.label(crate::i18n::fmt("Polygon vertices: {n}", &[("n", &app.boxes.other.vertices.len().to_string())]));
        if ui.button(tl!("Finish Polygon")).clicked()
            && let Some(id) = app.session.active_layer
        {
            finish_polygon(app, id);
        }
        if ui.button(tl!("Cancel Outline")).clicked() {
            cancel(app);
            app.boxes.error = None;
        }
        return;
    }
    let Ok((id, row, mut s)) = selected(app) else {
        ui.small(tl!("Click an annotation to select it. Hold Alt to create an overlapping annotation."));
        return;
    };
    if let Some(l) = app.session.map().layer(id)
        && let Some(fi) = l.dataset.field_index("class")
    {
        ui.label(crate::i18n::fmt("Selected class: {class}", &[("class", &l.dataset.value(row, fi).to_string())]));
    }
    let mut changed = false;
    match &mut s.shape {
        Shape::Circle { radius, .. } => {
            ui.small(tl!("Radius uses the stored reference grid: pixels for an image, or local grid units."));
            let label = ui.label(tl!("Radius"));
            let r = ui.add(egui::DragValue::new(radius).update_while_editing(false).speed(0.1)).labelled_by(label.id);
            changed = crate::widgets::changed(&r);
            if changed {
                crate::widgets::one_step_per_press(app, &r);
            }
        }
        Shape::Point { position } => {
            ui.small(tl!("Position in the stored reference grid."));
            for (i, name) in [(0, tl!("X")), (1, tl!("Y"))] {
                if let Some(v) = position.get_mut(i) {
                    let label = ui.label(name);
                    let r = ui.add(egui::DragValue::new(v).update_while_editing(false).speed(0.1)).labelled_by(label.id);
                    if crate::widgets::changed(&r) {
                        changed = true;
                        crate::widgets::one_step_per_press(app, &r);
                    }
                }
            }
        }
        Shape::Polygon { vertices } => {
            ui.label(crate::i18n::fmt("Vertices: {n}", &[("n", &vertices.len().to_string())]));
            ui.small(tl!("Drag a vertex to reshape; drag inside to move the polygon."));
        }
    }
    if changed {
        let _ = app.run("annotation.updateShape", json!({"layer":id,"row":row,"shape":s}));
    }
    if ui.button(tl!("Apply Class to Selected Annotation")).clicked() {
        let _ = app.run("annotation.updateShape", json!({"layer":id,"row":row,"shape":s,"class":app.boxes.class}));
    }
}
pub(crate) fn draw(app: &RustGisApp, p: &egui::Painter, rect: Rect) {
    let selected = selected(app).ok();
    let s = app.boxes.other.preview.as_ref().or_else(|| selected.as_ref().map(|x| &x.2));
    let class = if app.boxes.other.drag.as_ref().is_some_and(|g| g.row.is_none()) || !app.boxes.other.vertices.is_empty() {
        app.boxes.class.clone()
    } else {
        selected
            .as_ref()
            .and_then(|(id, row, _)| {
                app.session.map().layer(*id).and_then(|l| l.dataset.field_index("class").map(|i| l.dataset.value(*row, i).to_string()))
            })
            .unwrap_or_else(|| app.boxes.class.clone())
    };
    let colors = app.session.active_layer.map(|id| class_colors(&app.session, id)).unwrap_or_default();
    let c = colors.iter().find(|(n, _)| n == &class).map(|(_, c)| *c).unwrap_or_else(|| default_class_color(colors.len()));
    let color = if app.boxes.error.is_some() { Color32::RED } else { Color32::from_rgb(c.r(), c.g(), c.b()) };
    if let Some(f) = &app.boxes.other.frame
        && !app.boxes.other.vertices.is_empty()
    {
        let mut points = app.boxes.other.vertices.iter().filter_map(|v| screen(app, rect, f, *v)).collect::<Vec<_>>();
        for q in &points {
            p.circle_filled(*q, 4., color);
        }
        if let Some(q) = p.ctx().input(|i| i.pointer.hover_pos()).filter(|q| rect.contains(*q)) {
            points.push(q);
        }
        if points.len() > 1 {
            p.add(egui::Shape::line(points, Stroke::new(2., color)));
        }
        return;
    }
    let Some(s) = s else { return };
    if let Ok(g) = s.geometry() {
        match g {
            Geometry::Point(c) => {
                if let Some(q) = lonlat_to_screen(app, rect, c) {
                    p.circle_stroke(q, 8., Stroke::new(2., color));
                }
            }
            Geometry::Polygon(polys) => {
                if let Some(r) = polys.first().and_then(|p| p.first()) {
                    let points = r.iter().filter_map(|c| lonlat_to_screen(app, rect, *c)).collect::<Vec<_>>();
                    p.add(egui::Shape::line(points, Stroke::new(2., color)));
                }
            }
            _ => {}
        }
    }
    for (_, v) in handles(s) {
        if let Some(q) = screen(app, rect, &s.frame, v) {
            p.rect_filled(Rect::from_center_size(q, vec2(8., 8.)), 1., Color32::WHITE);
            p.rect_stroke(Rect::from_center_size(q, vec2(8., 8.)), 1., Stroke::new(1.5, color), egui::StrokeKind::Middle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;
    fn harness() -> egui_kittest::Harness<'static, RustGisApp> {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("annotation.newLayer", json!({})).unwrap();
        app.run("map.zoomToExtent", json!({"extent":[-0.005,-0.005,0.005,0.005]})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400., 900.)).with_step_dt(0.016).with_max_steps(10000).build_ui_state(
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
    fn pointer(h: &mut egui_kittest::Harness<'_, RustGisApp>, p: Pos2, down: Option<bool>) {
        h.event(egui::Event::PointerMoved(p));
        if let Some(pressed) = down {
            h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        }
        h.step();
    }
    fn click(h: &mut egui_kittest::Harness<'_, RustGisApp>, p: Pos2) {
        pointer(h, p, Some(true));
        pointer(h, p, Some(false));
    }
    fn drag(h: &mut egui_kittest::Harness<'_, RustGisApp>, a: Pos2, b: Pos2) {
        pointer(h, a, None);
        pointer(h, a, Some(true));
        pointer(h, b, None);
        pointer(h, b, Some(false));
    }
    fn key(h: &mut egui_kittest::Harness<'_, RustGisApp>, key: egui::Key, pressed: bool) {
        h.event(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    #[test]
    fn annotation_new_shapes_create_edit_undo_and_keep_one_layer() {
        let mut h = harness();
        let id = h.state().session.active_layer.unwrap();
        h.state_mut().boxes.class = "tree".into();
        h.state_mut().run("annotation.tool", json!({"shape":"point"})).unwrap();
        h.step();
        let rect = h.state().map.view_rect.unwrap().0;
        let c = rect.center() - vec2(180., 40.);
        click(&mut h, c);
        let before = selected(h.state()).unwrap().2;
        drag(&mut h, c, c + vec2(30., 20.));
        assert_ne!(selected(h.state()).unwrap().2, before);
        h.state_mut().run("edit.undo", json!({})).unwrap();
        h.step();
        assert_eq!(selected(h.state()).unwrap().2, before);
        h.state_mut().run("annotation.tool", json!({"shape":"circle"})).unwrap();
        h.step();
        let c = rect.center() + vec2(0., -50.);
        drag(&mut h, c, c + vec2(50., 0.));
        let s = selected(h.state()).unwrap().2;
        let Shape::Circle { radius, .. } = s.shape else { panic!("circle") };
        drag(&mut h, c + vec2(50., 0.), c + vec2(80., 0.));
        let Shape::Circle { radius: after, .. } = selected(h.state()).unwrap().2.shape else { panic!("circle") };
        assert!(after > radius);
        h.state_mut().run("annotation.tool", json!({"shape":"polygon"})).unwrap();
        h.step();
        let a = rect.center() + vec2(-100., 140.);
        let b = a + vec2(100., 0.);
        let c = a + vec2(40., 60.);
        for p in [a, b, c] {
            click(&mut h, p);
        }
        key(&mut h, egui::Key::Enter, true);
        key(&mut h, egui::Key::Enter, false);
        let before = selected(h.state()).unwrap().2;
        drag(&mut h, a, a - vec2(30., 0.));
        assert_ne!(selected(h.state()).unwrap().2, before);
        let l = h.state().session.map().layer(id).unwrap();
        assert_eq!(l.dataset.features.len(), 3);
        assert_eq!(h.state().session.map().layers.len(), 1);
        for row in 0..3 {
            assert_eq!(l.dataset.value(row, l.dataset.field_index("class").unwrap()).to_string(), "tree");
        }
        let centre = rect.center() + vec2(160., 140.);
        let frame = Frame::default();
        let at = local(h.state(), rect, centre, &frame).unwrap();
        let res = h.state().session.map().camera.res;
        let b = rustgis_engine::cmd::annotation::BoxSpec { version: 1, frame, center: at, width: 60. * res, height: 40. * res, angle: 30. };
        h.state_mut().run("annotation.addBox", json!({"box":b,"class":"tree"})).unwrap();
        h.state_mut().run("annotation.tool", json!({"shape":"point"})).unwrap();
        h.step();
        click(&mut h, centre);
        assert_eq!(h.state().session.tool, Tool::RotatedBox);
        click(&mut h, rect.center() + vec2(0., -50.));
        assert_eq!(h.state().session.tool, Tool::AnnotationCircle);
        click(&mut h, rect.center() - vec2(180., 40.));
        assert_eq!(h.state().session.tool, Tool::AnnotationPoint);
    }
    #[test]
    fn annotation_image_bound_ui_uses_original_pixels_for_circle_and_obb() {
        use rustgis_engine::data::annotation::{Image, Metadata};
        use rustgis_engine::data::raster::Crs;
        let mut h = harness();
        let id = h.state().session.active_layer.unwrap();
        let image = Image {
            file_name: "tile.tif".into(),
            width: 2048,
            height: 2048,
            crs: Crs::WebMercator,
            affine: [-1024., 1., 0.2, 1024., 0.1, -1.],
        };
        h.state_mut().session.map_mut().layer_mut(id).unwrap().dataset.annotation =
            Some(Metadata { version: 1, image: image.clone(), categories: Vec::new() });
        h.state_mut().boxes.class = "vehicle".into();
        h.state_mut().run("annotation.tool", json!({"shape":"circle"})).unwrap();
        h.step();
        let rect = h.state().map.view_rect.unwrap().0;
        let a = rect.center() - vec2(130., 80.);
        drag(&mut h, a, a + vec2(45., 0.));
        assert_eq!(selected(h.state()).unwrap().2.frame.affine, Some(image.affine));
        h.state_mut().run("annotation.tool", json!({"shape":"box"})).unwrap();
        h.step();
        let a = rect.center() + vec2(30., 70.);
        drag(&mut h, a, a + vec2(90., 50.));
        let boxes = h.state_mut().run("annotation.obb", json!({})).unwrap();
        assert_eq!(boxes["count"], 1);
        assert_eq!(boxes["boxes"][0]["obb"].as_array().unwrap().len(), 4);
        assert!(boxes["boxes"][0]["obb"][0][0].as_f64().unwrap() > 500.);
        assert_eq!(h.state().session.map().layer(id).unwrap().dataset.features.len(), 2);
        assert!(h.query_by_label("Image: tile.tif").is_some());
        let bytes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = bytes.clone();
        h.state_mut().services.download = Some(Box::new(move |_, value| {
            *captured.borrow_mut() = value;
            Ok(())
        }));
        h.state_mut().run("ui.exportLayer", json!({"format":"geojson"})).unwrap();
        let geo: serde_json::Value = serde_json::from_slice(&bytes.borrow()).unwrap();
        assert_eq!(geo["rustgis_annotation"]["image"]["file_name"], "tile.tif");
        assert_eq!(geo["rustgis_annotation"]["categories"][0]["name"], "vehicle");
        assert_eq!(geo["features"][1]["properties"]["obb"].as_array().unwrap().len(), 4);
    }
    #[test]
    fn annotation_small_circle_rim_resizes_without_moving_centre() {
        let mut h = harness();
        let rect = h.state().map.view_rect.unwrap().0;
        let c = rect.center();
        let frame = Frame::default();
        let centre = local(h.state(), rect, c, &frame).unwrap();
        let res = h.state().session.map().camera.res;
        let shape = ShapeSpec { version: 1, frame, shape: Shape::Circle { center: centre, radius: 5. * res } };
        h.state_mut().run("annotation.addShape", json!({"shape":shape,"class":"small"})).unwrap();
        h.state_mut().run("annotation.tool", json!({"shape":"circle"})).unwrap();
        h.step();
        drag(&mut h, c + vec2(5., 0.), c + vec2(30., 0.));
        let Shape::Circle { center, radius } = selected(h.state()).unwrap().2.shape else { panic!("circle") };
        assert_eq!(center, centre);
        assert!(radius > 20. * res);
    }
    #[test]
    fn annotation_polygon_pan_and_failed_finish_retain_draft() {
        let mut h = harness();
        h.state_mut().run("annotation.tool", json!({"shape":"polygon"})).unwrap();
        h.step();
        let rect = h.state().map.view_rect.unwrap().0;
        let a = rect.center() - vec2(100., 50.);
        click(&mut h, a);
        click(&mut h, a + vec2(90., 0.));
        key(&mut h, egui::Key::Space, true);
        drag(&mut h, a + vec2(150., 30.), a + vec2(190., 60.));
        key(&mut h, egui::Key::Space, false);
        assert_eq!(h.state().boxes.other.vertices.len(), 2);
        assert_eq!(h.state().session.tool, Tool::AnnotationPolygon);
        key(&mut h, egui::Key::Enter, true);
        key(&mut h, egui::Key::Enter, false);
        assert_eq!(h.state().boxes.other.vertices.len(), 2);
        assert!(h.state().boxes.error.is_some());
        click(&mut h, a + vec2(50., 90.));
        h.state_mut().boxes.class = "x".repeat(201);
        key(&mut h, egui::Key::Enter, true);
        key(&mut h, egui::Key::Enter, false);
        assert_eq!(h.state().boxes.other.vertices.len(), 3);
        h.state_mut().boxes.class = "building".into();
        key(&mut h, egui::Key::Enter, true);
        key(&mut h, egui::Key::Enter, false);
        assert!(h.state().boxes.other.vertices.is_empty());
        assert!(selected(h.state()).is_ok());
    }
}
