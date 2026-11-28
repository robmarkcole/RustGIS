//! Portable shortcut overrides and atomic, previewed merges into device preferences.
use crate::{
    RustGisApp, SHORTCUT_FILE_MAX_BYTES as MAX_FILE_BYTES, dialogs,
    keymap::{Change, Chord, Keymap},
    shortcutui,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    rustgis_shortcuts: u32,
    #[serde(deserialize_with = "import_bindings")]
    bindings: Vec<Change>,
}

/// Reject misspelled fields in portable files without changing tolerant preference loading.
fn import_bindings<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Change>, D::Error> {
    use serde::de::Error;
    Vec::<Value>::deserialize(deserializer)?
        .into_iter()
        .map(|record| {
            if let Some(fields) = record.as_object()
                && fields.keys().any(|key| !matches!(key.as_str(), "keys" | "command" | "params" | "remove"))
            {
                return Err(D::Error::custom(tl!("Unknown shortcut binding field.")));
            }
            serde_json::from_value(record).map_err(D::Error::custom)
        })
        .collect()
}

/// A validated document awaiting confirmation; the merge is recomputed against current bindings.
pub(crate) struct Preview {
    pub text: String,
    pub summary: Value,
}

/// Route picker and dropped shortcut bytes without allocating a base64 copy of oversized input.
pub(crate) fn picked(app: &mut RustGisApp, bytes: &[u8]) -> Result<Value, String> {
    picked_bytes(app, bytes).inspect_err(|error| app.session.notify(error.clone(), true))
}

/// Validate raw picker bytes before constructing command parameters.
fn picked_bytes(app: &mut RustGisApp, bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(tl!("Shortcut file exceeds the size limit.").into());
    }
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    run(app, "ui.shortcuts.import", &json!({"text":text}))
}

/// Read at most the portable file limit, including native files and browser picker payloads.
fn input(p: &Value) -> Result<String, String> {
    let bytes = if let Some(text) = p.get("text").and_then(Value::as_str) {
        if text.len() > MAX_FILE_BYTES {
            return Err(tl!("Shortcut file exceeds the size limit.").into());
        }
        return Ok(text.to_owned());
    } else if let Some(data) = p.get("data").and_then(Value::as_str) {
        use base64::Engine;
        if data.len() > MAX_FILE_BYTES.div_ceil(3) * 4 {
            return Err(tl!("Shortcut file exceeds the size limit.").into());
        }
        base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| e.to_string())?
    } else if let Some(path) = p.get("path").and_then(Value::as_str) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::io::Read;
            let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            file.take((MAX_FILE_BYTES + 1) as u64).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            bytes
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = path;
            return Err(tl!("Choose a shortcut file in the browser.").into());
        }
    } else {
        return Err(tl!("Provide shortcut file text, data or a path.").into());
    };
    if bytes.len() > MAX_FILE_BYTES {
        return Err(tl!("Shortcut file exceeds the size limit.").into());
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// Validate every record before exposing a preview or changing the live map.
fn merge(app: &RustGisApp, text: &str) -> Result<(Keymap, Value), String> {
    if text.len() > MAX_FILE_BYTES {
        return Err(tl!("Shortcut file exceeds the size limit.").into());
    }
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if value.get("rustgis_shortcuts").and_then(Value::as_u64).is_some_and(|version| version != 1) {
        return Err(tl!("Unsupported shortcut file version.").into());
    }
    let doc: Document = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if doc.rustgis_shortcuts != 1 {
        return Err(tl!("Unsupported shortcut file version.").into());
    }
    let changes = serde_json::to_string(&doc.bindings).map_err(|e| e.to_string())?;
    let mut merged = app.ui.keymap.clone();
    if !merged.apply_preferences(&changes) {
        return Err(tl!("Shortcut file contains invalid or reserved bindings.").into());
    }
    if merged.preferences(&Keymap::new(&app.session.registry)).is_none() {
        return Err(tl!("Too many shortcut changes to save.").into());
    }
    let mut assigned = std::collections::HashSet::new();
    for change in &doc.bindings {
        let chord = Chord::parse(&change.keys).ok_or(tl!("Invalid shortcut key."))?;
        if !change.remove {
            assigned.insert(chord);
        }
    }
    let unknown = assigned
        .iter()
        .filter_map(|&chord| merged.command_for(chord))
        .filter(|binding| !known(app, &binding.command))
        .map(|binding| &binding.command)
        .collect::<std::collections::HashSet<_>>()
        .len();
    let conflicts = assigned
        .into_iter()
        .filter(|&chord| {
            merged
                .command_for(chord)
                .is_some_and(|new| app.ui.keymap.command_for(chord).is_some_and(|old| !old.runs(&new.command, &new.params)))
        })
        .count();
    Ok((merged, json!({"bindings":doc.bindings.len(), "conflicts":conflicts, "unknown_commands":unknown})))
}

/// Include UI actions outside Command Search when recognizing portable command ids.
fn known(app: &RustGisApp, id: &str) -> bool {
    app.session.registry.get(id).is_some()
        || rustgis_engine::plugins::command(id).is_some()
        || crate::chrome::UI_COMMANDS.iter().any(|(c, _, _)| *c == id)
        || matches!(
            id,
            "ui.shortcuts"
                | "ui.commandSearch"
                | "ui.open"
                | "ui.save"
                | "ui.saveAs"
                | "ui.ribbon"
                | "ui.identify"
                | "ui.shortcuts.set"
                | "ui.shortcuts.reset"
                | "ui.about"
                | "ui.pane.contents"
        )
}

/// Export just the overrides, leaving future defaults free to arrive on another device.
fn document(app: &RustGisApp) -> Result<String, String> {
    let changes = app.ui.keymap.preferences(&Keymap::new(&app.session.registry)).ok_or(tl!("Too many shortcut changes to save."))?;
    let bindings = serde_json::from_str(&changes).map_err(|e| e.to_string())?;
    let text = serde_json::to_string(&Document { rustgis_shortcuts: 1, bindings }).map_err(|e| e.to_string())?;
    // Compact output and the metadata allowance let every valid saved configuration export.
    if text.len() > MAX_FILE_BYTES {
        return Err(tl!("Shortcut file exceeds the size limit.").into());
    }
    Ok(text)
}

/// UI command boundary: file import previews by default; confirmation commits a complete merge.
pub fn run(app: &mut RustGisApp, id: &str, p: &Value) -> Result<Value, String> {
    match id {
        "ui.shortcuts.import" => {
            if p.is_null() || p.as_object().is_some_and(|o| o.is_empty()) {
                if let Some(open) = &app.services.open_async {
                    app.ui.pending_open = None;
                    open("shortcuts");
                    return Ok(Value::Null);
                }
                if let Some(path) = dialogs::pick_open(&["json", "rgisshortcuts"], tl!("Keyboard shortcuts")) {
                    return run(app, id, &json!({"path":path}));
                }
                return Ok(Value::Null);
            }
            let text = input(p)?;
            let (merged, summary) = merge(app, &text)?;
            if p.get("confirm").and_then(Value::as_bool).unwrap_or(false) {
                if app.ui.shortcut_preferences_blocked {
                    return Err(tl!("Reset unreadable saved shortcuts before importing.").into());
                }
                app.ui.keymap = merged;
                shortcutui::save(app);
                app.ui.shortcut_editor.import = None;
            } else {
                app.ui.shortcut_editor.import = Some(Preview { text, summary: summary.clone() });
                app.ui.shortcut_editor.open = true;
            }
            Ok(summary)
        }
        "ui.shortcuts.export" => {
            let text = document(app)?;
            if let Some(path) = p.get("path").and_then(Value::as_str) {
                #[cfg(not(target_arch = "wasm32"))]
                rustgis_engine::data::io::write_atomic(std::path::Path::new(path), text.as_bytes()).map_err(|e| e.to_string())?;
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = path;
                    return Err(tl!("Use a browser download to export shortcuts.").into());
                }
            } else if !p.get("text").and_then(Value::as_bool).unwrap_or(false) {
                if let Some(download) = &app.services.download {
                    download("rustgis-shortcuts.json", std::sync::Arc::new(text.as_bytes().to_vec()))?;
                } else if let Some(path) = dialogs::pick_save("rustgis-shortcuts.json", &["json"], tl!("Keyboard shortcuts")) {
                    rustgis_engine::data::io::write_atomic(std::path::Path::new(&path), text.as_bytes()).map_err(|e| e.to_string())?;
                }
            }
            Ok(json!({"text":text}))
        }
        "ui.shortcuts.set" | "ui.shortcuts.reset" => {
            let defaults = Keymap::new(&app.session.registry);
            let mut next = app.ui.keymap.clone();
            if id == "ui.shortcuts.reset" && p.get("keys").is_none() {
                if !p.is_null() && !p.as_object().is_some_and(|o| o.is_empty()) {
                    return Err(tl!("Provide keys to reset, or no parameters to reset all.").into());
                }
                next = defaults.clone();
                app.ui.shortcut_preferences_blocked = false;
            } else {
                let keys = p.get("keys").and_then(Value::as_str).ok_or(tl!("Shortcut keys are required."))?;
                let chord =
                    Chord::parse(keys).filter(|c| !crate::keymap::is_clipboard(*c)).ok_or(tl!("Invalid or reserved shortcut key."))?;
                if id == "ui.shortcuts.reset" {
                    next.remove(chord);
                    if let Some(binding) = defaults.command_for(chord) {
                        next.assign(chord, &binding.command, binding.params.clone());
                    }
                } else {
                    let command = p.get("command").and_then(Value::as_str).ok_or(tl!("A command id is required."))?;
                    let change = json!([{
                        "keys":chord.to_string(), "command":command,
                        "params":p.get("params").cloned().unwrap_or(Value::Null),
                        "remove":p.get("remove").and_then(Value::as_bool).unwrap_or(false)
                    }]);
                    if !next.apply_preferences(&change.to_string()) {
                        return Err(tl!("Invalid shortcut binding.").into());
                    }
                }
            }
            if next.preferences(&defaults).is_none() {
                return Err(tl!("Too many shortcut changes to save.").into());
            }
            app.ui.keymap = next;
            shortcutui::save(app);
            app.ui.shortcut_editor.import = None;
            Ok(Value::Null)
        }
        _ => Err(tl!("Unknown shortcut command.").into()),
    }
}
