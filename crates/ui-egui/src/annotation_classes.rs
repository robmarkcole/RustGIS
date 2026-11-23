//! Class defaults are session-only. Values and colours use ordinary attributes and symbology.
use crate::{RightPane, RustGisApp};
use rustgis_engine::data::{FieldType, GeometryType};
use rustgis_engine::map::{Layer, Renderer};
use serde_json::{Value, json};

const MAX_CLASS_NAME_CHARS: usize = 256;

/// Normalise all entry points before storing a default, style or per-feature value.
fn class_name(value: &str) -> Result<String, String> {
    let name = value.trim();
    if name.is_empty() {
        return Err(tl!("Choose a non-empty class name.").into());
    }
    if name.chars().take(MAX_CLASS_NAME_CHARS + 1).count() > MAX_CLASS_NAME_CHARS {
        return Err(tl!("Class names may contain at most 256 characters.").into());
    }
    if rustgis_engine::data::Value::is_null_marker(name) {
        return Err(tl!("<Null> is reserved for missing values. Choose another class name.").into());
    }
    Ok(name.to_string())
}

fn styled(app: &RustGisApp, name: &str, value: &str) -> bool {
    target(app).is_some_and(|l| {
        matches!(&l.renderer, Renderer::Unique { field, classes, expression: None, .. }
            if field == name && classes.iter().any(|(existing, _)| existing == value))
    })
}

type Binding = (u64, u64, u32, u64, u64);
#[derive(Default)]
pub(crate) struct State {
    binding: Option<Binding>,
    field: Option<String>,
    value: Option<String>,
    draft: String,
    recent: Vec<String>,
    refusal: Option<String>,
}
fn target(app: &RustGisApp) -> Option<&Layer> {
    app.session.active_layer.and_then(|id| app.session.map().layer(id)).filter(|l| {
        !l.is_streamed() && !l.graphics && l.raster.is_none() && matches!(l.geometry_type(), GeometryType::Polygon | GeometryType::None)
    })
}
fn binding(app: &RustGisApp) -> Option<Binding> {
    let l = target(app)?;
    Some((app.session.opened, app.session.map().instance_id(), l.id, l.instance_id(), l.replacement_revision()))
}
fn creating(app: &RustGisApp) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        target(app).is_some_and(|l| app.reads.adding_class_field(&app.session, l.id))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = app;
        false
    }
}
pub(crate) fn sync(app: &mut RustGisApp) {
    let binding = binding(app);
    if app.annotation.classes.binding != binding {
        clear_refusal(app);
        let field = target(app).and_then(|l| {
            let mut matches = l.dataset.fields.iter().filter(|f| f.name.eq_ignore_ascii_case("class"));
            let field = matches.next()?;
            (field.ty == FieldType::Text && matches.next().is_none()).then(|| field.name.clone())
        });
        app.annotation.classes = State { binding, field, ..State::default() };
    } else if app.annotation.classes.refusal.is_some() && drawing_attributes(app).is_ok() {
        clear_refusal(app);
    }
}

pub(crate) fn refused(app: &mut RustGisApp, error: &str) {
    app.annotation.classes.refusal = Some(error.to_string());
}
fn clear_refusal(app: &mut RustGisApp) {
    let Some(error) = app.annotation.classes.refusal.take() else { return };
    if crate::boxedit::error(app) == Some(error.as_str()) {
        crate::boxedit::clear_idle_error(app);
    }
    if crate::circleedit::error(app) == Some(error.as_str()) {
        crate::circleedit::clear_idle_error(app);
    }
}
fn field(app: &RustGisApp) -> Result<&str, String> {
    let state = &app.annotation.classes;
    if state.binding != binding(app) {
        return Err(tl!("The target layer changed. Choose its class field again.").into());
    }
    let name = state.field.as_deref().ok_or_else(|| tl!("Choose a class field first.").to_string())?;
    if target(app).is_some_and(|l| l.dataset.fields.iter().filter(|f| f.name.eq_ignore_ascii_case(name)).count() > 1) {
        return Err(tl!("Class field names must be unique ignoring case. Rename the duplicate fields first.").into());
    }
    if !target(app).is_some_and(|l| l.dataset.fields.iter().any(|f| f.name == name && f.ty == FieldType::Text)) {
        return Err(tl!("The class field is missing or is no longer text. Choose another class field.").into());
    }
    Ok(name)
}
pub(crate) fn select_field(app: &mut RustGisApp, p: &Value) -> Result<(), String> {
    sync(app);
    let layer = target(app).ok_or_else(|| tl!("Choose a loaded editable polygon layer.").to_string())?;
    let name = match p.get("field") {
        Some(Value::Null) => None,
        Some(Value::String(name)) => {
            if layer.dataset.fields.iter().filter(|f| f.name.eq_ignore_ascii_case(name)).count() > 1 {
                return Err(tl!("Class field names must be unique ignoring case. Rename the duplicate fields first.").into());
            }
            if !layer.dataset.fields.iter().any(|f| f.name == *name && f.ty == FieldType::Text) {
                return Err(tl!("Choose an existing text field for classes.").into());
            }
            Some(name.clone())
        }
        _ => return Err(tl!("field must be a text field name or null.").into()),
    };
    if app.annotation.classes.field != name {
        app.annotation.classes.field = name;
        app.annotation.classes.value = None;
        app.annotation.classes.draft.clear();
        app.annotation.classes.recent.clear();
    }
    Ok(())
}
pub(crate) fn select_class(app: &mut RustGisApp, p: &Value) -> Result<(), String> {
    sync(app);
    let name = field(app)?.to_string();
    let value = match p.get("value") {
        Some(Value::Null) => None,
        Some(Value::String(value)) => Some(class_name(value)?),
        _ => return Err(tl!("Choose a non-empty class name, or null for no class default.").into()),
    };
    if let Some(value) = &value {
        if !styled(app, &name, value) {
            app.run("layer.classStyle", json!({"field":name,"value":value,"create":true}))?;
        }
        let recent = &mut app.annotation.classes.recent;
        recent.retain(|v| v != value);
        recent.insert(0, value.clone());
        recent.truncate(32);
    }
    app.annotation.classes.value = value;
    Ok(())
}
pub(crate) fn create_field(app: &mut RustGisApp) -> Result<(), String> {
    sync(app);
    let layer = target(app).ok_or_else(|| tl!("Choose a loaded editable polygon layer.").to_string())?;
    if layer.dataset.field_index("class").is_some() {
        return Err(tl!("A field named class already exists. Choose a text field, or rename the existing field in Data.").into());
    }
    if creating(app) {
        return Err(tl!("Class field creation is already running.").into());
    }
    let id = layer.id;
    app.run("edit.addField", json!({"layer":id,"name":"class","type":"text"}))?;
    // Desktop delivery is asynchronous. Select the requested name now; the pane only enables
    // class choices once the worker has installed that field. A target switch clears it.
    app.annotation.classes.field = Some("class".into());
    app.annotation.classes.value = None;
    app.annotation.classes.draft.clear();
    app.annotation.classes.recent.clear();
    Ok(())
}
/// Capture at pointer-down, so changing the active class never relabels a held preview.
/// No scan of features or file I/O takes place on the drawing path.
pub(crate) fn drawing_attributes(app: &RustGisApp) -> Result<Value, String> {
    if app.annotation.classes.field.as_deref() == Some("class") && creating(app) {
        return Err(tl!("Wait for the class field before drawing.").into());
    }
    let Some(value) = &app.annotation.classes.value else { return Ok(json!({})) };
    let name = field(app)?;
    let mut attrs = serde_json::Map::new();
    attrs.insert(name.to_string(), json!(value));
    Ok(Value::Object(attrs))
}
pub(crate) fn assign(app: &mut RustGisApp, p: &Value) -> Result<(), String> {
    sync(app);
    let name = field(app)?.to_string();
    let value = match p.get("value") {
        None => app.annotation.classes.value.clone().ok_or_else(|| tl!("Choose an active class first.").to_string())?,
        Some(Value::String(value)) => class_name(value)?,
        _ => return Err(tl!("Choose a non-empty class name.").into()),
    };
    let l = target(app).ok_or_else(|| tl!("Choose a loaded editable polygon layer.").to_string())?;
    let [row] = l.selection.as_slice() else { return Err(tl!("Select exactly one annotation to change its class.").into()) };
    let fi = l.dataset.fields.iter().position(|f| f.name == name).ok_or_else(|| tl!("Choose a class field first.").to_string())?;
    let feature = l.dataset.features.row(*row).ok_or_else(|| tl!("The selected annotation is no longer available.").to_string())?;
    if feature.value(fi) == &rustgis_engine::data::Value::Text(value.clone()) && styled(app, &name, &value) {
        return Ok(());
    }
    app.run("edit.setValue", json!({"layer":l.id,"row":row,"field":name,"value":value,"classStyle":true})).map(|_| ())
}
pub(crate) fn colours(app: &mut RustGisApp) -> Result<(), String> {
    sync(app);
    let name = field(app)?.to_string();
    let l = target(app).ok_or_else(|| tl!("Choose a loaded editable polygon layer.").to_string())?;
    let styled = matches!(&l.renderer, Renderer::Unique { field, expression: None, .. } if *field == name);
    app.ui.symbology_pending = if styled { None } else { Some(crate::PendingUniqueSymbology::new(&app.session, l, name)) };
    app.ui.open_right(RightPane::Symbology);
    Ok(())
}

fn choices(app: &RustGisApp) -> (std::collections::BTreeMap<String, Option<egui::Color32>>, bool) {
    // Renderer classes and recent choices are bounded suggestions, never a per-frame row scan.
    let mut choices = std::collections::BTreeMap::new();
    let l = target(app);
    let mut more = false;
    if let Some(Renderer::Unique { field, classes, expression: None, .. }) = l.map(|l| &l.renderer)
        && Some(field) == app.annotation.classes.field.as_ref()
    {
        more = classes.len() > 64;
        for (name, symbol) in classes.iter().take(64) {
            let c = symbol.fill;
            choices.entry(name.clone()).or_insert(Some(egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), c.a())));
        }
    }
    for name in &app.annotation.classes.recent {
        choices.entry(name.clone()).or_insert(None);
    }
    (choices, more)
}

/// One full-width hit target for the label, colour swatch and trailing space.
fn class_row(ui: &mut egui::Ui, name: &str, color: Option<egui::Color32>, selected: bool) -> egui::Response {
    let mut label = egui::text::LayoutJob::default();
    label.append(name, 22., egui::TextFormat::simple(egui::TextStyle::Button.resolve(ui.style()), egui::Color32::PLACEHOLDER));
    let response = ui.add(egui::Button::selectable(selected, label).min_size(egui::vec2(ui.available_width(), 24.)));
    if let Some(color) = color
        && ui.is_rect_visible(response.rect)
    {
        ui.painter().circle_filled(crate::widgets::menu_icon_rect(ui, &response).center(), 5., color);
    }
    response
}

/// Called only for the right-click target validated by the shared polygon menu.
pub(crate) fn menu(app: &mut RustGisApp, ui: &mut egui::Ui, layer: u32, row: usize) {
    sync(app);
    if app.session.active_layer != Some(layer) || target(app).is_none_or(|l| l.selection != [row]) {
        return;
    }
    let name = match field(app) {
        Ok(name) => name.to_string(),
        Err(_) => {
            ui.add_enabled(false, egui::Button::new(tl!("Change class")))
                .on_disabled_hover_text(tl!("Choose a text class field in Annotate first."));
            return;
        }
    };
    let current = target(app).and_then(|l| {
        let fi = l.dataset.fields.iter().position(|f| f.name == name)?;
        l.dataset.features.row(row).map(|feature| feature.value(fi).to_string())
    });
    let (choices, more) = choices(app);
    ui.menu_button(tl!("Change class"), |ui| {
        ui.set_max_width(220.);
        for (value, color) in &choices {
            if value.trim().is_empty() {
                continue;
            }
            if class_row(ui, value, *color, current.as_ref() == Some(value)).clicked() {
                let _ = app.run("ui.annotationAssignClass", json!({"value":value}));
                ui.close();
            }
        }
        if more {
            ui.small(tl!("Showing the first 64 styled classes and recent choices."));
        }
        ui.separator();
        if ui.button(tl!("Other class…")).clicked() {
            let _ = crate::annotation::open(app);
            ui.close();
        }
    });
}

pub(crate) fn panel(app: &mut RustGisApp, ui: &mut egui::Ui) {
    sync(app);
    let Some(l) = target(app) else { return };
    let fields: Vec<String> = l.dataset.fields.iter().filter(|f| f.ty == FieldType::Text).map(|f| f.name.clone()).collect();
    let can_create = l.dataset.field_index("class").is_none();
    let one_selected = l.selection.len() == 1;
    let current = app.annotation.classes.field.clone();
    let mut choice = current.clone();
    ui.separator();
    egui::ComboBox::from_label(tl!("Class field")).selected_text(choice.as_deref().unwrap_or(tl!("No class field"))).show_ui(ui, |ui| {
        ui.selectable_value(&mut choice, None, tl!("No class field"));
        for name in &fields {
            ui.selectable_value(&mut choice, Some(name.clone()), name);
        }
    });
    if choice != current {
        let _ = app.run("ui.annotationClassField", json!({"field":choice}));
    }
    if app.annotation.classes.field.is_none() {
        if can_create && ui.button(tl!("Create class field")).clicked() {
            let _ = app.run("ui.annotationCreateClassField", json!({}));
        }
        if !can_create && fields.is_empty() {
            ui.small(tl!("The existing class field is not text. Add a text field in Data, or rename the existing field first."));
        } else {
            ui.small(tl!("Choose a text field, or create class, to label new shapes."));
        }
        ui.separator();
        return;
    }
    if creating(app) && app.annotation.classes.field.as_deref() == Some("class") {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(tl!("Creating class field…"));
        });
        ui.separator();
        return;
    }
    if let Err(error) = field(app) {
        ui.colored_label(crate::theme::Tokens::get(ui.ctx()).red, error);
        if can_create && app.annotation.classes.field.as_deref() == Some("class") && ui.button(tl!("Retry class field creation")).clicked()
        {
            let _ = app.run("ui.annotationCreateClassField", json!({}));
        }
        ui.separator();
        return;
    }
    let (choices, more) = choices(app);
    let mut value = app.annotation.classes.value.clone();
    egui::ComboBox::from_label(tl!("Active class")).selected_text(value.as_deref().unwrap_or(tl!("No class default"))).show_ui(ui, |ui| {
        ui.set_max_width(220.);
        if class_row(ui, tl!("No class default"), None, value.is_none()).clicked() {
            value = None;
        }
        for (name, color) in &choices {
            if class_row(ui, name, *color, value.as_ref() == Some(name)).clicked() {
                value = Some(name.clone());
            }
        }
    });
    if value != app.annotation.classes.value {
        let _ = app.run("ui.annotationClass", json!({"value":value}));
    }
    if more {
        ui.small(tl!("Showing the first 64 styled classes and recent choices. Enter another class below."));
    }
    let new_class = ui.label(tl!("New class"));
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut app.annotation.classes.draft).desired_width(130.)).labelled_by(new_class.id);
        if ui.add_enabled(!app.annotation.classes.draft.trim().is_empty(), egui::Button::new(tl!("Use class"))).clicked() {
            let value = app.annotation.classes.draft.trim().to_string();
            if app.run("ui.annotationClass", json!({"value":value})).is_ok() {
                app.annotation.classes.draft.clear();
            }
        }
    });
    let selected = target(app).and_then(|l| {
        let [row] = l.selection.as_slice() else { return None };
        let fi = l.dataset.fields.iter().position(|f| Some(&f.name) == app.annotation.classes.field.as_ref())?;
        l.dataset.features.row(*row).map(|feature| feature.value(fi).to_string())
    });
    if let Some(value) = selected {
        ui.label(crate::i18n::fmt("Selected class: {value}", &[("value", &value)]));
    }
    ui.add_enabled_ui(one_selected && app.session.map().editing.settings.enabled && app.annotation.classes.value.is_some(), |ui| {
        if ui.button(tl!("Assign to selected annotation")).clicked() {
            let _ = app.run("ui.annotationAssignClass", json!({}));
        }
    });
    if ui.button(tl!("Class colours…")).clicked() {
        let _ = app.run("ui.annotationClassColours", json!({}));
    }
    ui.small(tl!("New shapes use the active class. New classes get a legend entry and an automatic colour; existing colours are kept. Class colours opens Symbology to customise them. GeoJSON keeps class values, not these colours."));
    ui.separator();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::{
        Project, Session,
        data::{Dataset, Field},
    };

    #[test]
    fn right_click_changes_only_the_clicked_shape_class_and_preserves_the_drawing_default() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::{Tool, box_edit::Frame, data::Value as Cell, geom::Coord};
        for (tool, row, center, value, pitch, reset) in [
            (Tool::Circle, 0, [-100000., 0.], "truck", 0., false),
            (Tool::RotatedBox, 1, [100000., 0.], "car", 0., false),
            (Tool::Select, 1, [100000., 0.], "car", 11.4, false),
            (Tool::Circle, 0, [-100000., 0.], "truck", 0., true),
            (Tool::Explore, 1, [100000., 0.], "car", 0., false),
        ] {
            let mut session = Session::new(Project::default());
            session.active_layer = Some(session.map_mut().add_dataset(Dataset::new(
                "Annotations",
                GeometryType::Polygon,
                vec![Field::new("class", FieldType::Text), Field::new("notes", FieldType::Text)],
            )));
            session.map_mut().camera.res = 1000.;
            session.map_mut().camera.center = Coord::new(0., 0.);
            let mut app = RustGisApp::new(session);
            app.run("edit.enable", json!({"on":true})).unwrap();
            app.run(
                "edit.addCircle",
                json!({"circle":{"center":[-100000.,0.],"radius":20000.,"tolerance":100.},"attributes":{"class":"car","notes":"keep"}}),
            )
            .unwrap();
            app.run(
                "edit.addBox",
                json!({"box":{"center":[100000.,0.],"width":40000.,"height":40000.,"angle":20.},"attributes":{"class":"truck"}}),
            )
            .unwrap();
            app.run("layer.symbology", json!({"type":"unique","field":"class"})).unwrap();
            if row == 0 {
                app.run("ui.annotationClass", json!({"value":"car"})).unwrap();
            }
            if reset {
                app.run("ui.annotationClass", json!({"value":"truck"})).unwrap();
                app.run("ui.annotationClass", json!({"value":"car"})).unwrap();
                app.run("layer.symbology", json!({"type":"single"})).unwrap();
            }
            if pitch != 0. {
                app.run("ui.pane.annotation", json!({})).unwrap();
                app.run("map.setBearing", json!({"bearing":17})).unwrap();
                app.run("map.setPitch", json!({"pitch":pitch})).unwrap();
            }
            app.run("tool.set", json!({"tool":tool.code()})).unwrap();
            let layer = app.session.active_layer.unwrap();
            app.run("select.rows", json!({"rows":[1-row]})).unwrap();
            let before = app.session.map().layer(layer).unwrap().dataset.clone();
            let before_renderer = app.session.map().layer(layer).unwrap().renderer.clone();
            let circle = app.session.circle_spec(layer, 0).unwrap();
            let bbox = app.session.box_spec(layer, 1).unwrap();
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000., 800.)).with_max_steps(1000).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
            for _ in 0..6 {
                h.step();
            }
            let rect = h.state().map.view_rect.unwrap().0;
            let at = crate::mapview::lonlat_to_screen(h.state(), rect, Frame::default().to_lonlat(center).unwrap()).unwrap();
            h.event(egui::Event::PointerMoved(at));
            for pressed in [true, false] {
                h.event(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
                h.step();
            }
            h.run();
            assert_eq!(
                h.state().session.map().layer(layer).unwrap().selection,
                vec![row],
                "right-click targets the clicked object, not the old selection"
            );
            if tool != Tool::Explore {
                assert!(h.query_by_label("Convert to Shape…").is_none(), "known annotation shapes keep their editing actions");
            }
            if tool == Tool::Explore {
                assert_ne!(h.state().ui.right, Some(RightPane::Annotation), "ordinary polygon editing also offers Change class");
            }
            let depth = h.state().session.undo_depth();
            h.get_by_label("Change class").click();
            h.run();
            let bounds = h.get_by_role_and_label(egui::accesskit::Role::Button, value).rect();
            let menu_row = h.get_by_label("Other class…").rect();
            let at = egui::pos2(if row == 0 { menu_row.left() + 10. } else { menu_row.right() - 3. }, bounds.center().y);
            assert!(bounds.contains(at), "the class button covers both the swatch and the menu's trailing space");
            h.event(egui::Event::PointerMoved(at));
            for pressed in [true, false] {
                h.event(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
                h.step();
            }
            h.run();
            assert!(h.query_by_label("Change class").is_none(), "applying a class closes the context menu");
            let data = &h.state().session.map().layer(layer).unwrap().dataset;
            assert_eq!(data.value(row, 0), &Cell::Text(value.into()));
            assert_eq!(data.features[1 - row], before.features[1 - row]);
            assert_eq!(data.features[row].id, before.features[row].id);
            assert_eq!(data.features[row].geometry, before.features[row].geometry);
            assert_eq!(data.value(row, 1), before.value(row, 1));
            assert_eq!(h.state().session.circle_spec(layer, 0), Some(circle));
            assert_eq!(h.state().session.box_spec(layer, 1), Some(bbox.clone()));
            assert_eq!(h.state().annotation.classes.value.as_deref(), if row == 0 { Some("car") } else { None });
            assert_eq!(h.state().session.undo_depth(), depth + 1);
            let after_renderer = h.state().session.map().layer(layer).unwrap().renderer.clone();
            if reset {
                assert!(after_renderer.legend().iter().any(|entry| entry.label == value), "a recent value restores its colour rule");
            }
            h.state_mut().run("edit.undo", json!({})).unwrap();
            assert_eq!(h.state().session.map().layer(layer).unwrap().renderer, before_renderer);
            assert_eq!(h.state().session.map().layer(layer).unwrap().dataset, before);
            h.state_mut().run("edit.redo", json!({})).unwrap();
            assert_eq!(h.state().session.map().layer(layer).unwrap().dataset.value(row, 0), &Cell::Text(value.into()));
            assert_eq!(h.state().session.map().layer(layer).unwrap().renderer, after_renderer);
            if pitch != 0. {
                assert!(!crate::boxedit::ready(h.state()));
                h.get_by_label("Reset map tilt").click();
                h.run();
                assert_eq!(h.state().session.map().camera.pitch, 0.);
                assert_eq!(h.state().session.map().camera.bearing, 17., "reset tilt keeps the chosen bearing");
                assert!(crate::boxedit::ready(h.state()));
                assert_eq!(h.state().session.box_spec(layer, 1), Some(bbox));
            }
        }
    }

    #[test]
    fn class_names_are_normalised_bounded_and_never_coerced_to_missing_values() {
        use rustgis_engine::data::Value as Cell;
        let mut data = Dataset::new("Classes", GeometryType::Polygon, vec![Field::new("class", FieldType::Text)]);
        data.push(None, vec![Cell::Text("seed".into())]);
        let mut session = Session::new(Project::default());
        let id = session.map_mut().add_dataset(data);
        session.active_layer = Some(id);
        let mut app = RustGisApp::new(session);
        app.run("edit.enable", json!({"on":true})).unwrap();
        app.run("select.rows", json!({"rows":[0]})).unwrap();
        app.run("ui.annotationClass", json!({"value":"  car  "})).unwrap();
        assert_eq!(app.annotation.classes.value.as_deref(), Some("car"));
        assert!(app.session.map().layer(id).unwrap().renderer.legend().iter().any(|entry| entry.label == "car"));
        app.run("ui.annotationAssignClass", json!({"value":"  truck  "})).unwrap();
        assert_eq!(app.session.map().layer(id).unwrap().dataset.value(0, 0), &Cell::Text("truck".into()));
        assert_eq!(app.annotation.classes.value.as_deref(), Some("car"));
        for invalid in ["<null>".into(), "  <NuLl>  ".into(), "".into(), "  ".into(), "a".repeat(257), "类".repeat(257)] {
            for command in ["ui.annotationClass", "ui.annotationAssignClass"] {
                let before = serde_json::to_value(&app.session.project).unwrap();
                let depth = app.session.undo_depth();
                let notices = app.session.notice_mark();
                assert!(app.run(command, json!({"value":invalid})).is_err());
                assert_eq!(serde_json::to_value(&app.session.project).unwrap(), before);
                assert_eq!(app.session.undo_depth(), depth);
                assert_eq!(app.annotation.classes.value.as_deref(), Some("car"));
                assert!(app.session.notices_since(notices).iter().any(|notice| notice.error), "command refusals already notify the user");
            }
        }
        let maximum = "类".repeat(256);
        app.run("ui.annotationClass", json!({"value":maximum})).unwrap();
        app.run("ui.annotationAssignClass", json!({})).unwrap();
        assert_eq!(app.session.map().layer(id).unwrap().dataset.value(0, 0), &Cell::Text(maximum));
    }

    #[test]
    fn class_field_defaults_are_unambiguous_optional_and_never_assign_a_value() {
        for (fields, expected) in [
            (vec![Field::new("class", FieldType::Text)], Some("class")),
            (vec![Field::new("Class", FieldType::Text)], Some("Class")),
            (vec![Field::new("name", FieldType::Text)], None),
            (vec![Field::new("class", FieldType::Integer)], None),
            (vec![Field::new("class", FieldType::Text), Field::new("Class", FieldType::Text)], None),
            (vec![Field::new("class", FieldType::Text), Field::new("Class", FieldType::Integer)], None),
        ] {
            let mut session = Session::new(Project::default());
            session.active_layer = Some(session.map_mut().add_dataset(Dataset::new("Annotations", GeometryType::Polygon, fields)));
            let mut app = RustGisApp::new(session);
            let before = serde_json::to_value(&app.session.project).unwrap();
            let depth = app.session.undo_depth();
            sync(&mut app);
            assert_eq!(app.annotation.classes.field.as_deref(), expected);
            assert_eq!(drawing_attributes(&app).unwrap(), json!({}), "the field default never guesses a class value");
            assert_eq!(serde_json::to_value(&app.session.project).unwrap(), before);
            assert_eq!(app.session.undo_depth(), depth);
            select_field(&mut app, &json!({"field":null})).unwrap();
            for _ in 0..3 {
                sync(&mut app);
            }
            assert!(app.annotation.classes.field.is_none(), "an explicit opt-out survives subsequent frames");
            if expected.is_some() {
                let layer = app.session.active_layer.unwrap();
                let other = app.session.map_mut().add_dataset(Dataset::new("Other", GeometryType::Polygon, vec![]));
                app.run("layer.select", json!({"layer":other})).unwrap();
                app.run("layer.select", json!({"layer":layer})).unwrap();
                assert_eq!(app.annotation.classes.field.as_deref(), expected, "a newly chosen target gets its own default");
            }
        }
    }
}
