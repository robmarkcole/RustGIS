//! A layer kept in the project and the file it was read from: whether its edits and the file are
//! in step (#1385).

use crate::{CmdResult, CommandSpec as C, Session};
use serde_json::Value;

pub fn commands() -> Vec<C> {
    vec![
        C::new("source.syncState", "Source File State", "Data › Data Source", sync_state).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).view().params(
            r#"{"layer"?: "name|id (default: the active layer): one read whole from a Shapefile or .geojson file"} — whether it has edits its file lacks ("edited"), the file changed since it was read ("sourceChanged"), both ("both") or neither ("inSync"), and which of the file's parts changed"#,
        ),
    ]
}

/// The key a check of layer `id`'s files made ahead is taken by (see [`super::reads`]).
#[cfg(not(target_arch = "wasm32"))]
fn ahead_key(id: u32) -> String {
    format!("syncState:{id}")
}

/// A check of a layer made ahead: the record, path and rows checked (the rows shared with the
/// layer, not copied), the rows' digest and what changed in the files.
#[cfg(not(target_arch = "wasm32"))]
struct Checked {
    synced: rustgis_data::sync::Synced,
    path: String,
    dataset: rustgis_data::Dataset,
    rows: Result<u64, String>,
    changed: Result<Vec<String>, String>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Checked {
    /// Whether it checked layer `l` as it is: the same record and path, and the same rows (the
    /// same allocation: an edit copies them), fields and coordinate system.
    fn of(&self, l: &rustgis_map::Layer) -> bool {
        l.synced.as_ref() == Some(&self.synced)
            && l.dataset.source.as_deref() == Some(self.path.as_str())
            && l.dataset.features.shares(&self.dataset.features)
            && l.dataset.fields == self.dataset.fields
            && l.dataset.crs == self.dataset.crs
    }
}

/// The check `source.syncState` makes before it runs in the desktop app: every part of the
/// file read and hashed, and the layer's rows hashed, on a thread of its own with progress and
/// Cancel.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn sync_ahead(s: &Session, p: &Value) -> Option<super::reads::ReadWork> {
    let id = s.layer_id(p).ok()?;
    let l = s.map().layer(id)?;
    let (synced, path, dataset) = (l.synced.clone()?, l.dataset.source.clone().filter(|p| local_file(p))?, l.dataset.clone());
    Some(super::reads::ahead("Source File State", ahead_key(id), move || {
        let changed = synced.files.changed(std::path::Path::new(&path)).map_err(|e| e.to_string());
        let rows = rustgis_data::sync::rows_digest(&dataset).map_err(|e| e.to_string());
        Checked { synced, path, dataset, rows, changed }
    }))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn sync_ahead(_: &Session, _: &Value) -> Option<super::reads::ReadWork> {
    None
}

/// Compares the layer and the file it was read from, each with the record of them made when it
/// was read: whether the layer's rows differ from the rows read (edits the file lacks), and which
/// parts of the file differ from the parts read. It doesn't compare the layer with the file as
/// it is now, so the same edit made to both shows as `both`. Each part of the size recorded is
/// read and hashed, and the rows are hashed, so the work grows with the file. The desktop app
/// does it on a thread of its own first ([`sync_ahead`]), with progress and **Cancel**, and the
/// command takes that check as it lands; when the layer changed meanwhile it says so rather than
/// check again here, which would hold the window. The assistant, scripts and the command line
/// check where they ask.
fn sync_state(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("the layer no longer exists")?;
    let (Some(synced), Some(path)) = (&l.synced, &l.dataset.source) else {
        if l.dataset.source.as_deref().is_some_and(rustgis_data::sync::is_syncable) && !l.is_streamed() {
            return Err(format!(
                "{} has no record of its file as it was read (it was added before RustGIS kept one, a part of the file couldn't be read, or an older RustGIS saved the project): add the file again to compare it",
                l.name
            ));
        }
        return Err(format!("{} wasn't read whole from a Shapefile or .geojson file, so there's no file to compare it with", l.name));
    };
    // A project from elsewhere names the path: only a local file of a kind a record is made for
    // is opened, never a network share (opening one can send the user's login to its host).
    if !local_file(path) {
        return Err(format!("{}: {path} isn't a Shapefile or .geojson file on this computer, so it isn't checked", l.name));
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (synced, path);
        Err("the web app can't read files on this computer".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (was, files, path) = (synced.rows, synced.files.clone(), path.clone());
        let ahead = super::reads::take::<Checked>(s, &ahead_key(id));
        let l = s.map().layer(id).ok_or("the layer no longer exists")?;
        let (rows, changed) = match ahead {
            Some(checked) if checked.of(l) => (checked.rows?, checked.changed?),
            // Checking again here would hold the window: the check made on its thread is dropped.
            Some(_) => return Err("the layer or its file's record changed while it was checked: check again".into()),
            None => (
                rustgis_data::sync::rows_digest(&l.dataset).map_err(|e| e.to_string())?,
                files.changed(std::path::Path::new(&path)).map_err(|e| e.to_string())?,
            ),
        };
        let edited = rows != was;
        let state = match (edited, !changed.is_empty()) {
            (false, false) => "inSync",
            (true, false) => "edited",
            (false, true) => "sourceChanged",
            (true, true) => "both",
        };
        Ok(serde_json::json!({"layer": id, "path": path, "state": state, "edited": edited, "changed": changed}))
    }
}

/// Whether `path` names a file on this computer a record is made for: a Shapefile or `.geojson`
/// file, not an address, a database or a network share (`\\host\share`, `//host/share`,
/// `/\host\share`, `\\?\UNC\host\share`). Windows' long form of a local path (`\\?\C:\…`) is one
/// on this computer.
fn local_file(path: &str) -> bool {
    let b = rustgis_map::paths::local_path(path).as_bytes();
    let sep = |i: usize| b.get(i).is_some_and(|c| matches!(c, b'/' | b'\\'));
    // Windows takes any two leading separators, of either kind, for a network share, unless they
    // begin the long form of a path on a drive.
    let drive = b.get(2) == Some(&b'?') && sep(3) && b.get(4).is_some_and(u8::is_ascii_alphabetic) && b.get(5) == Some(&b':') && sep(6);
    let share = sep(0) && sep(1) && !drive;
    rustgis_map::paths::is_local(path) && rustgis_data::sync::is_syncable(path) && !share
}
