//! Session-only image/grid choices shared by Annotate and the ordinary Edit tools.
use crate::{Landed, RightPane, RustGisApp};
use rustgis_engine::{Tool, box_edit::Frame, image_grid::Source};
use serde_json::{Value, json};

#[derive(Default)]
pub(crate) struct State {
    context: Option<(u64, u64)>,
    image: Option<u32>,
    image_chosen: bool,
    source: Option<Source>,
    pixels: bool,
    frame: Option<Frame>,
    pending: Option<u64>,
    next: u64,
    error: Option<String>,
}
fn cancel(app: &mut RustGisApp) {
    if let Some(request) = app.annotation.pending.take() {
        #[cfg(not(target_arch = "wasm32"))]
        app.reads.cancel_annotation_grid(request);
        #[cfg(target_arch = "wasm32")]
        let _ = request;
    }
    crate::boxedit::cancel(app);
    crate::circleedit::cancel(app);
}
pub(crate) fn sync(app: &mut RustGisApp) {
    let context = (app.session.opened, app.session.map().instance_id());
    if app.annotation.context != Some(context) {
        cancel(app);
        app.annotation = State { context: Some(context), next: app.annotation.next, ..State::default() };
    } else if app.annotation.source.as_ref().is_some_and(|s| !s.current(&app.session)) {
        cancel(app);
        app.annotation.frame = None;
        app.annotation.source = None;
        app.annotation.error = Some(tl!("The image changed or was removed. Select it again or reload its grid.").into());
    }
}
pub(crate) fn open(app: &mut RustGisApp) -> Result<(), String> {
    app.ui.open_right(RightPane::Annotation);
    if !app.annotation.image_chosen
        && app.annotation.image.is_none()
        && let Some(id) = app.session.active_layer.filter(|id| app.session.map().layer(*id).is_some_and(|l| l.raster.is_some()))
    {
        select_image(app, &json!({"image":id}))?;
    }
    Ok(())
}
pub(crate) fn select_image(app: &mut RustGisApp, p: &Value) -> Result<(), String> {
    sync(app);
    let image = p.get("image").filter(|v| !v.is_null()).map(|v| app.session.layer_id(&json!({"layer":v}))).transpose()?;
    let source = image.map(|id| Source::capture(&app.session, id)).transpose()?;
    let request = app.annotation.next.checked_add(1).ok_or("image-grid request limit reached")?;
    cancel(app);
    app.annotation.next = request;
    app.annotation.image = image;
    app.annotation.image_chosen = true;
    app.annotation.source = source;
    app.annotation.pixels = image.is_some();
    app.annotation.frame = None;
    app.annotation.error = None;
    app.annotation.pending = image.map(|_| request);
    if let Some(id) = image {
        #[cfg(not(target_arch = "wasm32"))]
        let _ = crate::readjob::run_image_grid(app, json!({"layer":id}), request);
        #[cfg(target_arch = "wasm32")]
        {
            let result = app.run("raster.imageGrid", json!({"layer":id}));
            landed(app, request, Landed::Ran(result));
        }
    }
    Ok(())
}
pub(crate) fn select_grid(app: &mut RustGisApp, pixels: bool) -> Result<(), String> {
    sync(app);
    if pixels && app.annotation.image.is_none() {
        return Err(tl!("Choose an image before using original-image pixels.").into());
    }
    if pixels && app.annotation.frame.is_none() {
        return select_image(app, &json!({"image":app.annotation.image}));
    }
    cancel(app);
    app.annotation.pixels = pixels;
    app.annotation.error = None;
    Ok(())
}
pub(crate) fn cancel_read(app: &mut RustGisApp) {
    cancel(app);
    app.annotation.error = Some(tl!("Grid read cancelled. Reload the grid to draw in image pixels.").into());
}
pub(crate) fn landed(app: &mut RustGisApp, request: u64, result: Landed) {
    if app.annotation.pending != Some(request) {
        return;
    }
    sync(app);
    if app.annotation.pending != Some(request) {
        return;
    }
    app.annotation.pending = None;
    match result {
        Landed::Ran(Ok(value)) => match serde_json::from_value(value) {
            Ok(frame) => app.annotation.frame = Some(frame),
            Err(e) => app.annotation.error = Some(e.to_string()),
        },
        Landed::Ran(Err(e)) | Landed::NotRun(e) => app.annotation.error = Some(e),
    }
}
/// Only new objects use this choice. Existing objects keep their own frozen frame.
pub(crate) fn drawing_frame(app: &RustGisApp) -> Result<Frame, String> {
    if app.annotation.context != Some((app.session.opened, app.session.map().instance_id())) || !app.annotation.pixels {
        return Ok(Frame::default());
    }
    if !app.annotation.source.as_ref().is_some_and(|s| s.current(&app.session)) {
        return Err(tl!("The image changed. Reload its grid before drawing.").into());
    }
    app.annotation
        .frame
        .clone()
        .ok_or_else(|| app.annotation.error.clone().unwrap_or_else(|| tl!("Wait for the original image grid before drawing.").into()))
}
fn polygon_layer(app: &RustGisApp, id: u32) -> bool {
    app.session.map().layer(id).is_some_and(|l| {
        !l.is_streamed()
            && !l.graphics
            && l.raster.is_none()
            && matches!(l.geometry_type(), rustgis_engine::data::GeometryType::Polygon | rustgis_engine::data::GeometryType::None)
    })
}
pub(crate) fn select_layer(app: &mut RustGisApp, p: &Value) -> Result<(), String> {
    let id = app.session.layer_id(p)?;
    if !polygon_layer(app, id) {
        return Err(tl!("Choose a loaded editable polygon layer.").into());
    }
    app.run("layer.select", json!({"layer":id})).map(|_| ())
}
pub(crate) fn grid_controls(app: &mut RustGisApp, ui: &mut egui::Ui) {
    sync(app);
    let mut image = app.annotation.image;
    let mut picked = false;
    let name = image
        .and_then(|id| app.session.map().layer(id))
        .filter(|_| app.annotation.source.as_ref().is_some_and(|s| s.current(&app.session)))
        .map_or_else(|| tl!("No image").to_string(), |l| l.name.clone());
    egui::ComboBox::from_label(tl!("Image")).selected_text(name).show_ui(ui, |ui| {
        picked |= ui.selectable_value(&mut image, None, tl!("No image")).clicked();
        for l in app.session.map().layers.iter().filter(|l| l.raster.is_some()) {
            picked |= ui.selectable_value(&mut image, Some(l.id), &l.name).clicked();
        }
    });
    if picked {
        let _ = app.run("ui.annotationImage", json!({"image":image}));
    }
    let mut pixels = app.annotation.pixels;
    egui::ComboBox::from_label(tl!("New shape grid"))
        .selected_text(if pixels { tl!("Original image pixels") } else { tl!("Web Mercator grid metres") })
        .show_ui(ui, |ui| {
            ui.add_enabled_ui(image.is_some(), |ui| ui.selectable_value(&mut pixels, true, tl!("Original image pixels")));
            ui.selectable_value(&mut pixels, false, tl!("Web Mercator grid metres"));
        });
    if pixels != app.annotation.pixels {
        let _ = app.run("ui.annotationGrid", json!({"pixels":pixels}));
    }
    if app.annotation.pixels {
        ui.small(tl!("New shapes use the original GeoTIFF pixels, including rotation and shear. Existing objects keep their own grid."));
        if let Some(f) = &app.annotation.frame {
            ui.small(crate::i18n::fmt(
                "{width} × {height} pixels · EPSG:{epsg}",
                &[("width", &f.width.to_string()), ("height", &f.height.to_string()), ("epsg", &f.epsg.to_string())],
            ));
        }
        if app.annotation.pending.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tl!("Reading original image grid…"));
            });
            if ui.button(tl!("Cancel Grid Read")).clicked() {
                let _ = app.run("ui.annotationCancel", Value::Null);
            }
        } else if ui.button(tl!("Reload Grid")).clicked() {
            let _ = app.run("ui.annotationImage", json!({"image":image}));
        }
    } else {
        ui.small(tl!("Web Mercator grid metres, not ground metres. Image pixels are the default when an image is selected."));
    }
    if let Some(error) = &app.annotation.error {
        ui.colored_label(egui::Color32::RED, error);
    }
}
pub(crate) fn panel(app: &mut RustGisApp, ui: &mut egui::Ui) {
    ui.heading(tl!("Image annotation"));
    grid_controls(app, ui);
    ui.separator();
    let active = app.session.active_layer.filter(|id| polygon_layer(app, *id));
    let mut layer = active;
    let name =
        layer.and_then(|id| app.session.map().layer(id)).map_or_else(|| tl!("Choose a polygon layer").to_string(), |l| l.name.clone());
    egui::ComboBox::from_label(tl!("Target layer")).selected_text(name).show_ui(ui, |ui| {
        for l in app.session.map().layers.iter().filter(|l| polygon_layer(app, l.id)) {
            ui.selectable_value(&mut layer, Some(l.id), &l.name);
        }
    });
    if layer != active
        && let Some(id) = layer
    {
        let _ = app.run("ui.annotationLayer", json!({"layer":id}));
    }
    if layer.is_none() {
        ui.label(tl!("Add or create a polygon feature layer using Data, then choose it here."));
    }
    let mut enabled = app.session.map().editing.settings.enabled;
    if ui.checkbox(&mut enabled, tl!("Enable editing")).changed() {
        let _ = app.run("edit.enable", json!({"on":enabled}));
    }
    let ready = crate::boxedit::ready(app) && drawing_frame(app).is_ok();
    ui.add_enabled_ui(ready, |ui| {
        ui.horizontal(|ui| {
            for (tool, name) in [(Tool::RotatedBox, tl!("Rotated Box")), (Tool::Circle, tl!("Circle"))] {
                if ui.selectable_label(app.session.tool == tool, name).clicked() {
                    let _ = app.run("tool.set", json!({"tool":tool.code()}));
                }
            }
        });
    });
    if !crate::boxedit::ready(app) {
        ui.small(tl!("Drawing requires editing enabled, a loaded polygon layer and a flat Earth map with zero pitch."));
    }
    ui.small(tl!("Space or middle-drag pans; releasing Space returns to the active drawing tool. Undo/Redo use the existing commands."));
    crate::boxedit::panel(app, ui);
    crate::circleedit::panel(app, ui);
    ui.separator();
    ui.add_enabled_ui(layer.is_some(), |ui| {
        if ui.button(tl!("Export GeoJSON…")).clicked() {
            let _ = app.run("ui.exportLayer", json!({"format":"geojson"}));
        }
    });
    ui.small(tl!(
        "Exports contain ordinary polygons and feature properties. Image/grid choices and exact constructions remain session-only."
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster};
    use rustgis_engine::{
        Project, Session,
        data::{Dataset, GeometryType},
        map::RasterLayer,
    };

    // A minimal uncompressed TIFF with the original ModelTransformation, not a north-up
    // display raster. Encoded here so the end-to-end test needs no external image or library.
    fn tiff(frame: &Frame) -> Vec<u8> {
        let matrix_at = 8 + 2 + 11 * 12 + 4;
        let keys_at = matrix_at + 16 * 8;
        let pixels_at = keys_at + 16 * 2;
        let entries: [(u16, u16, u32, u32); 11] = [
            (256, 4, 1, frame.width),
            (257, 4, 1, frame.height),
            (258, 3, 1, 8),
            (259, 3, 1, 1),
            (262, 3, 1, 1),
            (273, 4, 1, pixels_at),
            (277, 3, 1, 1),
            (278, 4, 1, frame.height),
            (279, 4, 1, frame.width * frame.height),
            (34264, 12, 16, matrix_at),
            (34735, 3, 16, keys_at),
        ];
        let mut out = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
        out.extend(11_u16.to_le_bytes());
        for (tag, kind, count, value) in entries {
            out.extend(tag.to_le_bytes());
            out.extend(kind.to_le_bytes());
            out.extend(count.to_le_bytes());
            out.extend(value.to_le_bytes());
        }
        out.extend(0_u32.to_le_bytes());
        let a = frame.affine;
        for v in [a[1], a[2], 0., a[0], a[4], a[5], 0., a[3], 0., 0., 1., 0., 0., 0., 0., 1.] {
            out.extend(v.to_le_bytes());
        }
        for v in [1_u16, 1, 0, 3, 1024, 0, 1, 1, 1025, 0, 1, 1, 3072, 0, 1, frame.epsg as u16] {
            out.extend(v.to_le_bytes());
        }
        out.resize(pixels_at as usize + (frame.width * frame.height) as usize, 100);
        out
    }
    fn app() -> (RustGisApp, u32, u32, Frame) {
        let frame = Frame { epsg: 3857, width: 128, height: 128, affine: [0., 2., 0.4, 2000., 0.2, -3.] };
        let display = Raster::new(
            "overview",
            1,
            1,
            vec![vec![100.]],
            GeoTransform { origin_x: 0., origin_y: 2000., pixel_width: 1000., pixel_height: -1000. },
            Crs::from_epsg(3857).unwrap(),
            None,
            ColorHint::Gray,
            "UInt8",
        )
        .unwrap();
        let mut s = Session::new(Project::default());
        let image = s.map_mut().add_raster("Test TIFF", RasterLayer::from_bytes(display, "test.tif", tiff(&frame)));
        let layer = s.map_mut().add_dataset(Dataset::new("Objects", GeometryType::Polygon, vec![]));
        s.active_layer = Some(layer);
        s.map_mut().camera.center = frame.to_lonlat([64., 64.]).unwrap();
        s.map_mut().camera.res = 1.;
        s.run("edit.enable", &json!({"on":true})).unwrap();
        (RustGisApp::new(s), image, layer, frame)
    }
    fn screen(app: &RustGisApp, frame: &Frame, point: [f64; 2]) -> egui::Pos2 {
        crate::mapview::lonlat_to_screen(app, rect(), frame.to_lonlat(point).unwrap()).unwrap()
    }
    fn rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000., 800.))
    }
    fn drag(app: &mut RustGisApp, frame: &Frame, start: [f64; 2], end: [f64; 2], new: bool) {
        let start = screen(app, frame, start);
        let end = screen(app, frame, end);
        if app.session.tool == Tool::Circle {
            assert!(crate::circleedit::drag_start(app, rect(), start, new));
            crate::circleedit::drag_to(app, rect(), end);
            crate::circleedit::drag_stop(app);
        } else {
            assert!(crate::boxedit::drag_start(app, rect(), start, new));
            crate::boxedit::drag_to(app, rect(), end, false);
            crate::boxedit::drag_stop(app);
        }
    }
    #[test]
    fn annotation_grid_defaults_to_original_header_and_explicit_mercator_is_respected() {
        let (mut app, image, layer, frame) = app();
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        assert_eq!(drawing_frame(&app).unwrap(), frame);
        assert_eq!(app.session.active_layer, Some(layer));
        app.run("ui.annotationGrid", json!({"pixels":false})).unwrap();
        assert_eq!(drawing_frame(&app).unwrap(), Frame::default());
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        assert_eq!(drawing_frame(&app).unwrap(), frame);
        app.run("ui.annotationImage", json!({"image":null})).unwrap();
        assert_eq!(drawing_frame(&app).unwrap(), Frame::default());
        app.run("layer.select", json!({"layer":image})).unwrap();
        open(&mut app).unwrap();
        assert!(app.annotation.image.is_none(), "reopening the pane must respect an explicit No image choice");
    }
    #[test]
    fn annotation_grid_drags_in_rotated_sheared_pixels_and_edits_keep_their_frame() {
        for tool in [Tool::Circle, Tool::RotatedBox] {
            let (mut app, image, layer, frame) = app();
            app.run("ui.annotationImage", json!({"image":image})).unwrap();
            app.run("tool.set", json!({"tool":tool.code()})).unwrap();
            let depth = app.session.undo_depth();
            drag(&mut app, &frame, [20., 20.], [80., 60.], true);
            assert_eq!(app.session.undo_depth(), depth + 1);
            if tool == Tool::Circle {
                let c = app.session.circle_spec(layer, 0).unwrap();
                assert_eq!(c.frame, frame);
                assert!((c.center[0] - 20.).abs() < 0.01 && (c.radius - 60_f64.hypot(40.)).abs() < 0.01);
            } else {
                let b = app.session.box_spec(layer, 0).unwrap();
                assert_eq!(b.frame, frame);
                assert!((b.width - 60.).abs() < 0.01 && (b.height - 40.).abs() < 0.01 && (b.center[0] - 50.).abs() < 0.01);
            }
            app.run("edit.undo", json!({})).unwrap();
            assert!(app.session.map().layer(layer).unwrap().dataset.features.is_empty());
            assert_eq!(drawing_frame(&app).unwrap(), frame);
            app.run("edit.redo", json!({})).unwrap();
            assert_eq!(drawing_frame(&app).unwrap(), frame);
            app.run("ui.annotationGrid", json!({"pixels":false})).unwrap();
            if tool == Tool::Circle {
                let c = app.session.circle_spec(layer, 0).unwrap().clone();
                drag(&mut app, &frame, [c.center[0] + c.radius, c.center[1]], [c.center[0] + 100., c.center[1]], false);
                let edited = app.session.circle_spec(layer, 0).unwrap();
                assert_eq!(edited.frame, frame);
                assert!((edited.radius - 100.).abs() < 0.01);
            } else {
                drag(&mut app, &frame, [80., 60.], [90., 70.], false);
                let edited = app.session.box_spec(layer, 0).unwrap();
                assert_eq!(edited.frame, frame);
                assert!((edited.width - 70.).abs() < 0.01 && (edited.height - 50.).abs() < 0.01);
            }
        }
    }
    #[test]
    fn annotation_grid_failures_and_replaced_sources_never_fall_back_silently() {
        let (mut app, image, _, _) = app();
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        app.session.map_mut().layer_mut(image).unwrap().raster.as_mut().unwrap().embedded = Some(std::sync::Arc::new(vec![0; 16]));
        sync(&mut app);
        assert!(app.annotation.source.is_none());
        assert!(drawing_frame(&app).is_err());
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        assert!(app.annotation.error.is_some() && drawing_frame(&app).is_err());
        app.run("tool.set", json!({"tool":"circle"})).unwrap();
        assert!(!crate::circleedit::drag_start(&mut app, rect(), egui::pos2(400., 350.), true));
        app.run("ui.annotationGrid", json!({"pixels":false})).unwrap();
        assert_eq!(drawing_frame(&app).unwrap(), Frame::default());
    }
    #[test]
    fn annotation_grid_source_changes_during_a_drag_cannot_commit_in_the_old_grid() {
        for tool in [Tool::Circle, Tool::RotatedBox] {
            let (mut app, image, layer, frame) = app();
            app.run("ui.annotationImage", json!({"image":image})).unwrap();
            app.run("tool.set", json!({"tool":tool.code()})).unwrap();
            let start = screen(&app, &frame, [20., 20.]);
            let end = screen(&app, &frame, [80., 60.]);
            let depth = app.session.undo_depth();
            if tool == Tool::Circle {
                assert!(crate::circleedit::drag_start(&mut app, rect(), start, true));
                crate::circleedit::drag_to(&mut app, rect(), end);
            } else {
                assert!(crate::boxedit::drag_start(&mut app, rect(), start, true));
                crate::boxedit::drag_to(&mut app, rect(), end, false);
            }
            // Also protects a source replacement received directly through MCP before UI sync.
            app.session.map_mut().layer_mut(image).unwrap().raster.as_mut().unwrap().embedded = Some(std::sync::Arc::new(vec![0; 32]));
            if tool == Tool::Circle {
                crate::circleedit::drag_stop(&mut app);
            } else {
                crate::boxedit::drag_stop(&mut app);
            }
            assert!(app.session.map().layer(layer).unwrap().dataset.features.is_empty());
            assert_eq!(app.session.undo_depth(), depth);
        }
    }
    #[test]
    fn annotation_grid_replaced_requests_ignore_old_failures_and_cancellation() {
        let (mut app, image, _, frame) = app();
        app.reads_off_frame = true;
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        let old = app.annotation.pending.unwrap();
        app.run("ui.annotationImage", json!({"image":image})).unwrap();
        let current = app.annotation.pending.unwrap();
        assert_ne!(old, current);
        for r in [
            Landed::Ran(Err("old failure".into())),
            Landed::NotRun("old cancel".into()),
            Landed::Ran(Ok(serde_json::to_value(&frame).unwrap())),
        ] {
            landed(&mut app, old, r);
            assert_eq!(app.annotation.pending, Some(current));
            assert!(app.annotation.error.is_none());
        }
        app.run("ui.annotationCancel", Value::Null).unwrap();
        landed(&mut app, current, Landed::Ran(Ok(serde_json::to_value(&frame).unwrap())));
        assert!(drawing_frame(&app).is_err());
        app.run("project.new", json!({})).unwrap();
        assert!(app.annotation.image.is_none());
        assert_eq!(drawing_frame(&app).unwrap(), Frame::default());
    }
    #[test]
    fn annotation_grid_pane_entry_defaults_and_layer_selection_share_commands() {
        use egui_kittest::kittest::Queryable;
        let (mut app, image, layer, _) = app();
        app.run("layer.select", json!({"layer":image})).unwrap();
        app.run("ui.tab", json!({"tab":"Analyze"})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1366., 900.)).with_max_steps(1000).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Annotate").click();
        h.run();
        assert_eq!(h.state().ui.right, Some(RightPane::Annotation));
        assert!(h.query_all_by_role(egui::accesskit::Role::ComboBox).any(|n| n.value().as_deref() == Some("Original image pixels")));
        h.get_by_label("Target layer").click();
        h.run();
        h.query_all_by_role_and_label(egui::accesskit::Role::Button, "Objects").find(|n| n.rect().min.x > 800.).unwrap().click();
        h.run();
        assert_eq!(h.state().session.active_layer, Some(layer));
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Circle").click();
        h.run();
        assert_eq!(h.state().session.tool, Tool::Circle);
    }
}
