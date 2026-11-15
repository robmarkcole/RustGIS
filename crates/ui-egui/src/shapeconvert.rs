//! Explicit, cancellable previews of polygon-to-shape replacement, using the shared read queue.
use crate::{RightPane, RustGisApp};
use rustgis_engine::shape_conversion::{Proposal, Shape};
use rustgis_engine::{data::Feature, geom::Coord};
use serde_json::{Value, json};

#[derive(Default)]
pub(crate) struct State {
    menu: Option<MenuTarget>,
    form: Option<Form>,
    next_request: u64,
}
#[derive(Clone, Copy)]
struct MenuTarget {
    layer: u32,
    row: usize,
    map: u64,
    instance: u64,
    revision: u64,
    replacement: u64,
    feature: u64,
}
impl MenuTarget {
    fn current(self, app: &RustGisApp) -> bool {
        app.session.map().instance_id() == self.map
            && app.session.map().layer(self.layer).is_some_and(|l| {
                l.instance_id() == self.instance
                    && l.geometry_rev == self.revision
                    && l.replacement_revision() == self.replacement
                    && l.selection == [self.row]
                    && l.dataset.features.get(self.row).is_some_and(|f| f.id == self.feature)
            })
    }
}
struct Form {
    previous_tool: rustgis_engine::Tool,
    image_stamp: Option<(u64, u64, u64)>,
    request: u64,
    layer: u32,
    row: usize,
    map: u64,
    instance: u64,
    replacement: u64,
    rows: std::sync::Weak<Vec<Feature>>,
    kind: String,
    image: Option<u32>,
    pending: bool,
    proposal: Option<Proposal>,
    error: Option<String>,
}
fn valid(app: &RustGisApp, f: &Form) -> bool {
    app.session.map().instance_id() == f.map
        && app.session.active_layer == Some(f.layer)
        && f.image.is_none_or(|id| {
            app.session.map().layer(id).is_some_and(|l| {
                l.raster.is_some() && f.image_stamp.is_none_or(|stamp| stamp == (l.instance_id(), l.replacement_revision(), l.geometry_rev))
            })
        })
        && rustgis_engine::box_edit::check_tool(&app.session, f.layer).is_ok()
        && app.session.map().layer(f.layer).is_some_and(|l| {
            l.instance_id() == f.instance
                && l.replacement_revision() == f.replacement
                && l.selection == [f.row]
                && l.dataset.features.allocation_key().ptr_eq(&f.rows)
        })
}
pub(crate) fn pick(app: &mut RustGisApp, c: Coord, alt: bool) {
    app.conversion.menu = None;
    if !crate::boxedit::ready(app) {
        return;
    }
    let hits = crate::boxedit::hits(app, c);
    let current = app.session.active_layer.and_then(|id| {
        let [row] = app.session.map().layer(id)?.selection.as_slice() else { return None };
        hits.iter().position(|h| *h == (id, *row))
    });
    let index = if alt { current.map_or(0, |i| (i + 1) % hits.len().max(1)) } else { current.unwrap_or(0) };
    if let Some(&(layer, row)) = hits.get(index) {
        let _ = app.run("select.rows", json!({"layer":layer,"rows":[row],"mode":"new"}));
        if let Some(l) = app.session.map().layer(layer)
            && let Some(f) = l.dataset.features.get(row)
        {
            app.conversion.menu = Some(MenuTarget {
                layer,
                row,
                map: app.session.map().instance_id(),
                instance: l.instance_id(),
                revision: l.geometry_rev,
                replacement: l.replacement_revision(),
                feature: f.id,
            });
        }
    } else if matches!(app.session.tool, rustgis_engine::Tool::Circle | rustgis_engine::Tool::RotatedBox) {
        let _ = app.run("select.clear", json!({}));
    }
}

pub(crate) fn open(app: &mut RustGisApp, p: &Value) -> Result<(), String> {
    let layer = app.session.layer_id(p)?;
    rustgis_engine::box_edit::check_tool(&app.session, layer)?;
    let l = app.session.map().layer(layer).ok_or("no layer")?;
    let [row] = l.selection.as_slice() else { return Err(tl!("Select one polygon to convert.").into()) };
    let kind = p.get("kind").and_then(Value::as_str).unwrap_or("circle");
    if !matches!(kind, "circle" | "rotatedBox") {
        return Err(tl!("Choose Circle or Rotated Box.").into());
    }
    app.conversion.next_request = app.conversion.next_request.checked_add(1).ok_or("conversion request limit reached")?;
    app.conversion.form = Some(Form {
        previous_tool: app.session.tool,
        image_stamp: None,
        request: app.conversion.next_request,
        layer,
        row: *row,
        map: app.session.map().instance_id(),
        instance: l.instance_id(),
        replacement: l.replacement_revision(),
        rows: l.dataset.features.allocation_key(),
        kind: kind.into(),
        image: None,
        pending: false,
        proposal: None,
        error: None,
    });
    let _ = app.run("tool.set", json!({"tool":"explore"}));
    app.map.cancel_sketch();
    app.ui.open_right(RightPane::Editing);
    Ok(())
}
pub(crate) fn menu(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let Some(target) = app.conversion.menu.filter(|t| t.current(app)) else { return };
    let layer = target.layer;
    ui.menu_button(tl!("Convert to"), |ui| {
        for (kind, label) in [("circle", tl!("Circle…")), ("rotatedBox", tl!("Rotated Box…"))] {
            if ui.button(label).clicked() {
                let _ = app.run("ui.convertShape", json!({"layer":layer,"kind":kind}));
                ui.close();
            }
        }
    });
}
pub(crate) fn landed(app: &mut RustGisApp, result: crate::Landed) {
    let Some(mut f) = app.conversion.form.take() else { return };
    if !valid(app, &f) {
        app.session.notify(tl!("Conversion preview closed because the source or selection changed."), false);
        return;
    }
    if let crate::Landed::Ran(Ok(ref answer)) = result
        && answer.get("request").and_then(Value::as_u64) != Some(f.request)
    {
        app.conversion.form = Some(f);
        return;
    }
    if !matches!(result, crate::Landed::Ran(Ok(_)))
        && (!f.pending || {
            #[cfg(not(target_arch = "wasm32"))]
            {
                app.reads.shape_conversion_waiting(f.request)
            }
            #[cfg(target_arch = "wasm32")]
            {
                false
            }
        })
    {
        app.conversion.form = Some(f);
        return;
    }
    f.pending = false;
    match result {
        crate::Landed::Ran(Ok(answer)) => match serde_json::from_value::<Proposal>(answer) {
            Ok(p) if p.layer == f.layer && p.row == f.row && p.current(&app.session) => f.proposal = Some(p),
            _ => f.error = Some(tl!("The source changed. Preview the conversion again.").into()),
        },
        crate::Landed::Ran(Err(e)) | crate::Landed::NotRun(e) => f.error = Some(e),
    }
    app.conversion.form = Some(f);
}
pub(crate) fn panel(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let Some(mut f) = app.conversion.form.take() else {
        if let Some(layer) = app.session.active_layer
            && app.session.map().layer(layer).is_some_and(|l| l.selection.len() == 1)
            && crate::boxedit::ready(app)
            && ui.button(tl!("Convert Selected Polygon…")).clicked()
        {
            let _ = app.run("ui.convertShape", json!({"layer":layer}));
        }
        return;
    };
    if !valid(app, &f) {
        app.session.notify(tl!("Conversion preview closed because the source or selection changed."), false);
        return;
    }
    ui.heading(tl!("Convert Polygon"));
    ui.small(tl!("Replaces only this polygon. Its ID and attributes are preserved. Apply is undoable."));
    let mut changed = false;
    ui.add_enabled_ui(!f.pending, |ui| {
        egui::ComboBox::from_label(tl!("Shape"))
            .selected_text(if f.kind == "circle" { tl!("Circle") } else { tl!("Rotated Box") })
            .show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut f.kind, "circle".into(), tl!("Circle")).changed();
                changed |= ui.selectable_value(&mut f.kind, "rotatedBox".into(), tl!("Rotated Box")).changed();
            });
        let title = f
            .image
            .and_then(|id| app.session.map().layer(id))
            .map(|l| l.name.clone())
            .unwrap_or_else(|| tl!("Web Mercator grid metres").into());
        egui::ComboBox::from_label(tl!("Construction Grid")).selected_text(title).show_ui(ui, |ui| {
            changed |= ui.selectable_value(&mut f.image, None, tl!("Web Mercator grid metres")).changed();
            for l in app.session.map().layers.iter().filter(|l| l.raster.is_some()) {
                changed |= ui.selectable_value(&mut f.image, Some(l.id), &l.name).changed();
            }
        });
    });
    if changed {
        f.proposal = None;
        f.image_stamp = None;
        f.error = None;
    }
    ui.small(if f.image.is_some() {
        tl!("Original GeoTIFF pixels, including rotation and shear. Only local or embedded GeoTIFFs are supported.")
    } else {
        tl!("Web Mercator grid metres, not ground metres.")
    });
    ui.label(if f.kind == "circle" {
        tl!("Fit: centre of the vertex bounds, radius enclosing all vertices. This is not recovery of the original circle.")
    } else {
        tl!("Fit: minimum-area enclosing rectangle of the vertices in the chosen grid.")
    });
    ui.small(tl!("The preview replaces the boundary; curved grid edges remain straight-edge approximations after projection. Saved/exported results are ordinary polygons."));
    if f.pending {
        ui.spinner();
        ui.label(tl!("Fitting in progress. Cancel is also available in the status bar."));
    }
    let preview = ui.add_enabled(!f.pending, egui::Button::new(tl!("Preview Conversion"))).clicked();
    if let Some(p) = &mut f.proposal {
        let frame = match &p.shape {
            Shape::Circle(c) => &c.frame,
            Shape::RotatedBox(b) => &b.frame,
        };
        ui.label(crate::i18n::fmt("Grid EPSG:{epsg}", &[("epsg", &frame.epsg.to_string())]));
        match &mut p.shape {
            Shape::Circle(c) => {
                number(ui, tl!("Center X"), &mut c.center[0]);
                number(ui, tl!("Center Y"), &mut c.center[1]);
                number(ui, tl!("Radius"), &mut c.radius);
                number(ui, tl!("Tolerance"), &mut c.tolerance);
            }
            Shape::RotatedBox(b) => {
                number(ui, tl!("Center X"), &mut b.center[0]);
                number(ui, tl!("Center Y"), &mut b.center[1]);
                number(ui, tl!("Width"), &mut b.width);
                number(ui, tl!("Height"), &mut b.height);
                number(ui, tl!("Angle"), &mut b.angle);
            }
        }
        if let Err(e) = p.shape.geometry() {
            ui.colored_label(egui::Color32::RED, e);
        }
    }
    if let Some(e) = &f.error {
        ui.colored_label(egui::Color32::RED, e);
    }
    let can_apply = !f.pending && f.proposal.as_ref().is_some_and(|p| p.shape.geometry().is_ok());
    let apply = ui.add_enabled(can_apply, egui::Button::new(tl!("Apply Conversion"))).clicked();
    if ui.button(tl!("Cancel Conversion")).clicked() {
        #[cfg(not(target_arch = "wasm32"))]
        app.reads.cancel_shape_conversion();
        if app.session.tool == rustgis_engine::Tool::Explore {
            let _ = app.run("tool.set", json!({"tool":f.previous_tool.code()}));
        }
        return;
    }
    ui.separator();
    if apply && let Some(p) = &f.proposal {
        let params = json!({"preview":p,"confirm":true});
        match app.run("edit.applyShapeConversion", params) {
            Ok(_) => {
                let _ = app.run("tool.set", json!({"tool":f.kind}));
                return;
            }
            Err(e) => f.error = Some(e),
        }
    }
    let params = preview.then(|| {
        app.conversion.next_request = app.conversion.next_request.saturating_add(1);
        f.request = app.conversion.next_request;
        f.image_stamp =
            f.image.and_then(|id| app.session.map().layer(id)).map(|l| (l.instance_id(), l.replacement_revision(), l.geometry_rev));
        f.pending = true;
        f.proposal = None;
        f.error = None;
        let mut p = json!({"layer":f.layer,"row":f.row,"kind":f.kind,"request":f.request});
        if let Some(image) = f.image {
            p["image"] = json!(image);
        }
        p
    });
    app.conversion.form = Some(f);
    if let Some(p) = params {
        let _ = app.run("ui.previewShapeConversion", p);
    }
}
fn number(ui: &mut egui::Ui, label: &str, value: &mut f64) {
    ui.horizontal(|ui| {
        let label = ui.label(label);
        ui.add(egui::DragValue::new(value).speed(0.1)).labelled_by(label.id);
    });
}
pub(crate) fn draw(app: &RustGisApp, painter: &egui::Painter, rect: egui::Rect) {
    let Some(f) = app.conversion.form.as_ref().filter(|f| valid(app, f)) else { return };
    let Some(p) = &f.proposal else { return };
    let Ok(rustgis_engine::data::Geometry::Polygon(polys)) = p.shape.geometry() else { return };
    let Some(ring) = polys.first().and_then(|p| p.first()) else { return };
    let points: Option<Vec<egui::Pos2>> = ring.iter().map(|c| crate::mapview::lonlat_to_screen(app, rect, *c)).collect();
    if let Some(points) = points {
        painter.add(egui::Shape::closed_line(points, egui::Stroke::new(2.5, egui::Color32::from_rgb(255, 155, 30))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::{
        Project, Session, Tool,
        box_edit::{BoxSpec, Frame},
        data::{Dataset, GeometryType},
    };
    pub(super) fn app() -> RustGisApp {
        let mut s = Session::new(Project::default());
        let id = s.map_mut().add_dataset(Dataset::new("Imported", GeometryType::Polygon, vec![]));
        s.active_layer = Some(id);
        s.run("edit.enable", &json!({"on":true})).unwrap();
        let b = BoxSpec { frame: Frame::default(), center: [1000., 1000.], width: 200., height: 80., angle: 25. };
        s.run("edit.addFeature", &json!({"geometry":rustgis_engine::data::io::geometry_to_geojson(&b.geometry().unwrap())})).unwrap();
        let mut app = RustGisApp::new(s);
        open(&mut app, &json!({"kind":"rotatedBox"})).unwrap();
        app
    }
    fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(500., 1100.))
            .with_max_steps(1000)
            .build_ui_state(|ui, app| panel(app, ui), app)
    }
    #[test]
    fn shape_conversion_preview_cancel_and_apply_use_the_existing_editor() {
        for apply in [false, true] {
            let app = app();
            let id = app.session.active_layer.unwrap();
            let before = app.session.map().layer(id).unwrap().dataset.features[0].clone();
            let depth = app.session.undo_depth();
            let mut h = harness(app);
            h.get_by_label("Preview Conversion").click();
            h.run();
            assert!(h.state().conversion.form.as_ref().unwrap().proposal.is_some());
            assert_eq!(h.state().session.undo_depth(), depth);
            assert_eq!(h.state().session.map().layer(id).unwrap().dataset.features[0], before);
            h.get_by_label(if apply { "Apply Conversion" } else { "Cancel Conversion" }).click();
            h.run();
            assert!(h.state().conversion.form.is_none());
            assert_eq!(h.state().session.undo_depth(), depth + usize::from(apply));
            assert_eq!(h.state().session.box_spec(id, 0).is_some(), apply);
            if apply {
                assert_eq!(h.state().session.tool, Tool::RotatedBox);
            }
        }
    }
    #[test]
    fn shape_conversion_selection_changes_and_late_answers_never_retarget() {
        let mut app = app();
        let request = app.conversion.form.as_ref().unwrap().request;
        let answer = app.session.run("edit.previewShapeConversion", &json!({"row":0,"kind":"circle","request":request})).unwrap();
        open(&mut app, &json!({"kind":"rotatedBox"})).unwrap();
        landed(&mut app, crate::Landed::Ran(Ok(answer)));
        assert!(app.conversion.form.as_ref().unwrap().proposal.is_none());
        app.session.run("select.clear", &json!({})).unwrap();
        let mut h = harness(app);
        h.run();
        assert!(h.state().conversion.form.is_none());
    }
}

#[cfg(test)]
mod overlap_tests {
    use super::*;
    #[test]
    fn shape_conversion_right_click_keeps_selected_overlap_and_alt_cycles() {
        let mut app = super::tests::app();
        let id = app.session.active_layer.unwrap();
        let g = app.session.map().layer(id).unwrap().dataset.features[0].geometry.clone().unwrap();
        app.session.run("edit.addFeature", &json!({"geometry":rustgis_engine::data::io::geometry_to_geojson(&g)})).unwrap();
        app.session.run("select.rows", &json!({"rows":[0],"mode":"new"})).unwrap();
        let c = rustgis_engine::box_edit::Frame::default().to_lonlat([1000., 1000.]).unwrap();
        pick(&mut app, c, false);
        assert_eq!(app.conversion.menu.map(|t| (t.layer, t.row)), Some((id, 0)));
        pick(&mut app, c, true);
        assert_eq!(app.conversion.menu.map(|t| (t.layer, t.row)), Some((id, 1)));
    }
}
