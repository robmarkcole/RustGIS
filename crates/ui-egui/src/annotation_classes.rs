//! Class defaults are session-only. Values and colours use ordinary attributes and symbology.
use crate::{RightPane, RustGisApp};
use rustgis_engine::data::{FieldType, GeometryType};
use rustgis_engine::map::{Layer, Renderer};
use serde_json::{Value, json};

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
        app.annotation.classes = State { binding, ..State::default() };
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
    field(app)?;
    let value = match p.get("value") {
        Some(Value::Null) => None,
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        _ => return Err(tl!("Choose a non-empty class name, or null for no class default.").into()),
    };
    if let Some(value) = &value {
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
pub(crate) fn assign(app: &mut RustGisApp) -> Result<(), String> {
    sync(app);
    let name = field(app)?.to_string();
    let value = app.annotation.classes.value.clone().ok_or_else(|| tl!("Choose an active class first.").to_string())?;
    let l = target(app).ok_or_else(|| tl!("Choose a loaded editable polygon layer.").to_string())?;
    let [row] = l.selection.as_slice() else { return Err(tl!("Select exactly one annotation to change its class.").into()) };
    let fi = l.dataset.fields.iter().position(|f| f.name == name).ok_or_else(|| tl!("Choose a class field first.").to_string())?;
    let feature = l.dataset.features.row(*row).ok_or_else(|| tl!("The selected annotation is no longer available.").to_string())?;
    if feature.value(fi) == &rustgis_engine::data::Value::Text(value.clone()) {
        return Ok(());
    }
    app.run("edit.setValue", json!({"layer":l.id,"row":row,"field":name,"value":value})).map(|_| ())
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
        ui.colored_label(egui::Color32::RED, error);
        if can_create && app.annotation.classes.field.as_deref() == Some("class") && ui.button(tl!("Retry class field creation")).clicked()
        {
            let _ = app.run("ui.annotationCreateClassField", json!({}));
        }
        ui.separator();
        return;
    }
    // Renderer classes and recent choices are bounded suggestions, never a per-frame row scan.
    let mut choices = std::collections::BTreeMap::new();
    let l = target(app);
    let mut more = false;
    if let Some(Renderer::Unique { field, classes, expression: None, .. }) = l.map(|l| &l.renderer)
        && Some(field) == app.annotation.classes.field.as_ref()
    {
        more = classes.len() > 64;
        for (name, symbol) in classes.iter().take(64) {
            choices.entry(name.clone()).or_insert(Some(symbol.fill));
        }
    }
    for name in &app.annotation.classes.recent {
        choices.entry(name.clone()).or_insert(None);
    }
    let mut value = app.annotation.classes.value.clone();
    egui::ComboBox::from_label(tl!("Active class")).selected_text(value.as_deref().unwrap_or(tl!("No class default"))).show_ui(ui, |ui| {
        ui.selectable_value(&mut value, None, tl!("No class default"));
        for (name, color) in &choices {
            ui.horizontal(|ui| {
                if let Some(c) = color {
                    ui.colored_label(egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), c.a()), "●");
                }
                ui.selectable_value(&mut value, Some(name.clone()), name);
            });
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
    ui.small(tl!("New shapes use the active class; no default keeps template values. Class colours opens Unique Values in Symbology; apply it to include new values. GeoJSON keeps class values, not these colours."));
    ui.separator();
}
