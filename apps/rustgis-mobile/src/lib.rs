//! RustGIS on Android (#1902).
//!
//! The desktop front end (`rustgis-ui-egui`) in a `NativeActivity`, with the phone's file picker
//! ([`phone`], a `rustgis_engine::platform::Platform`) standing where the browser's stands for
//! the web app. See `README.md` for how to build and install it.
//!
//! Not here yet: location and the camera; preferences and the keychain; autosave; a touch-first
//! front end.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_os = "android")]
mod phone;

/// The files of `picked` to open, each by its path: all but the parts another of them reads from
/// beside it (a Shapefile's `.dbf`, `.shx`, `.prj` and `.cpg`, a MapInfo file's `.mid`), matched
/// by name whatever the case. A part picked without its main file stays, and is refused by name.
#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
fn without_sidecars(picked: Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
    let lower = |part: Option<&std::ffi::OsStr>| part.map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default();
    let named: Vec<(String, String)> = picked.iter().map(|p| (lower(p.file_stem()), lower(p.extension()))).collect();
    let has = |stem: &str, extension: &str| named.iter().any(|(s, e)| s == stem && e == extension);
    let mut kept = Vec::new();
    for (path, (stem, extension)) in picked.iter().zip(&named) {
        let read_by = match extension.as_str() {
            "dbf" | "shx" | "prj" | "cpg" => "shp",
            "mid" => "mif",
            _ => "",
        };
        if read_by.is_empty() || !has(stem, read_by) {
            kept.push(path.clone());
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::without_sidecars;
    use std::path::PathBuf;

    fn kept(names: &[&str]) -> Vec<String> {
        without_sidecars(names.iter().map(|n| PathBuf::from("/picked/1").join(n)).collect())
            .iter()
            .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            .collect()
    }

    #[test]
    fn parts_picked_with_their_file_are_left_to_it() {
        assert_eq!(kept(&["roads.shp", "roads.dbf", "roads.shx", "roads.prj", "roads.cpg"]), ["roads.shp"]);
        assert_eq!(kept(&["ROADS.SHP", "Roads.DBF", "parcels.mif", "parcels.MID"]), ["ROADS.SHP", "parcels.mif"]);
    }

    #[test]
    fn a_part_picked_alone_is_kept_to_be_refused_by_name() {
        assert_eq!(kept(&["roads.dbf", "lakes.shp", "notes.prj"]), ["roads.dbf", "lakes.shp", "notes.prj"]);
        // Other files are never parts.
        assert_eq!(kept(&["a.gpkg", "a.geojson"]), ["a.gpkg", "a.geojson"]);
    }
}

#[cfg(target_os = "android")]
mod android {
    use crate::phone::Phone;
    use eframe::egui;
    use eframe::wgpu;
    use rustgis_engine::data::dirs;
    use rustgis_engine::platform::{Event, Platform};
    use rustgis_ui_egui::{Inbox, RustGisApp, i18n};
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use winit::platform::android::activity::AndroidApp;

    /// What a service couldn't do, kept until the next frame says it in the window (a service has
    /// no session to tell).
    type Refusals = Rc<RefCell<Vec<String>>>;

    /// The app: the desktop front end, with the files to open once the window runs.
    struct App {
        app: RustGisApp,
        /// The files pushed to the app's folder ([`pushed`]), until the first frame takes them.
        pushed: Vec<std::path::PathBuf>,
        /// How soon (milliseconds) the window draws again to take what is still to arrive from
        /// Android; `None` when nothing is.
        poll: Option<u64>,
        /// Whether the phone was last asked for positions (`RustGisApp::locate`, as last seen).
        locating: bool,
        /// Whether the system bars' icons were last asked to be dark (a light ground); `None`
        /// before the first frame.
        bars_light: Option<bool>,
        /// The phone's file picker, where its Java half is there.
        phone: Option<Rc<Phone>>,
        /// Picked files the front end reads as bytes (`Services::inbox`).
        inbox: Inbox,
        refusals: Refusals,
        /// Files picked for a dialog (purpose, name, copy), until their picker has answered in
        /// full: a Shapefile's parts arrive one by one and are read together.
        waiting: Vec<(String, String, PathBuf)>,
        /// What reading them off the window's thread found, until the next frame says it.
        problems: Problems,
        /// The batches being read.
        reading: Arc<AtomicUsize>,
    }

    /// Why a picked file didn't reach the dialog that asked for it.
    enum Problem {
        /// The shortcut file `name` is over `SHORTCUT_FILE_MAX_BYTES`.
        ShortcutTooLarge { name: String },
        /// The file `name` is over the size read into memory (`rustgis_data::io::MAX_BYTES`), by
        /// itself or `together` with the files picked with it and already read.
        TooLarge { name: String, together: bool },
        /// The copy of the file `name` couldn't be read.
        Unread { name: String, error: String },
        /// What gathering a Shapefile's parts found, in its own words.
        Shapefile(String),
    }

    type Problems = Arc<Mutex<Vec<Problem>>>;

    impl App {
        /// Takes what the phone did since the last frame: picked files are opened, the rest is
        /// said in the window.
        fn take_phone_events(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
            for text in self.refusals.borrow_mut().drain(..) {
                self.app.session.notify(text, true);
            }
            let Some(phone) = self.phone.clone() else { return };
            // Positioning follows what the interface asks for; off, the last position is let go.
            if self.app.locate != self.locating {
                self.locating = self.app.locate;
                if let Err(e) = phone.locate(self.locating) {
                    self.app.session.notify(e, true);
                    self.app.locate = false;
                    self.locating = false;
                }
                if !self.locating {
                    self.app.position = None;
                }
            }
            for event in phone.take_events() {
                match event {
                    Event::Picked { purpose, name, path } => self.waiting.push((purpose, name, path)),
                    Event::Cancelled => {}
                    Event::Saved { name } => self.app.session.notify(i18n::fmt("Saved {name}.", &[("name", &name)]), false),
                    Event::Position(fix) if self.locating => self.app.position = Some(fix),
                    // A fix that was on its way when positioning was turned off.
                    Event::Position(_) => {}
                    // Android ended positioning itself (its reason is a failure beside this): the
                    // interface goes back to offering to show the position.
                    Event::Stopped => {
                        self.app.locate = false;
                        self.locating = false;
                        self.app.position = None;
                    }
                    Event::Failed(text) => self.app.session.notify(text, true),
                }
            }
            for problem in self.problems.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
                let text = match problem {
                    Problem::ShortcutTooLarge { name } => {
                        let limit = rustgis_ui_egui::SHORTCUT_FILE_MAX_BYTES.to_string();
                        i18n::fmt("{name} wasn't opened: a shortcut file is at most {limit} bytes.", &[("name", &name), ("limit", &limit)])
                    }
                    Problem::TooLarge { name, together } => {
                        let limit = (rustgis_engine::data::io::MAX_BYTES >> 20).to_string();
                        let template = if together {
                            "{name} wasn't opened: with the files picked with it, it is over {limit} MiB."
                        } else {
                            "{name} wasn't opened: it is over {limit} MiB."
                        };
                        i18n::fmt(template, &[("name", &name), ("limit", &limit)])
                    }
                    Problem::Unread { name, error } => i18n::fmt("{name} wasn't opened: {error}", &[("name", &name), ("error", &error)]),
                    Problem::Shapefile(text) => text,
                };
                self.app.session.notify(text, true);
            }
            // A picked file is copied on a thread of Android's: look again until it has arrived.
            let busy = phone.busy();
            if !busy && !self.waiting.is_empty() {
                self.open_waiting(ctx, raw_input);
            }
            // Positions arrive on a thread of Android's too: looked for while they are wanted.
            // Asked for from inside the frame (`ui`): asked from here, before it, it isn't kept.
            // Soon for what is quick (a file being copied or read); once a second for positions,
            // which come no faster: redrawing the map more often than that would only cost battery.
            self.poll = if busy || self.reading.load(Ordering::Relaxed) > 0 {
                Some(250)
            } else if self.locating {
                Some(1000)
            } else {
                None
            };
        }

        /// Opens the picked files, now that every picker has answered in full, what was picked
        /// together kept together (its copies share a folder).
        ///
        /// Data and projects open by path, as files dropped on the desktop window do: a
        /// GeoPackage is then edited in place, in its copy, and a file read with the one beside
        /// it finds it. Every other picker's files go where the browser's do, to the dialog that
        /// asked: read into the inbox off the window's thread (a file can be large), a Shapefile's
        /// loose parts gathered into one layer. That read has no Cancel: each file is one read,
        /// and the dialogs' files are small beside data.
        fn open_waiting(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
            let mut batches: Vec<(Option<PathBuf>, String, Vec<(String, PathBuf)>)> = Vec::new();
            for (purpose, name, path) in self.waiting.drain(..) {
                let folder = path.parent().map(PathBuf::from);
                match batches.iter_mut().find(|(f, _, _)| *f == folder) {
                    Some((_, _, files)) => files.push((name, path)),
                    None => batches.push((folder, purpose, vec![(name, path)])),
                }
            }
            for (_, purpose, files) in batches {
                if purpose == "data" || purpose == "project" {
                    // A Shapefile's or a MapInfo file's parts picked with it are read by it, from
                    // beside it: opened on their own they would each be refused.
                    let paths = crate::without_sidecars(files.into_iter().map(|(_, path)| path).collect());
                    raw_input.dropped_files.extend(paths.into_iter().map(|path| std::sync::Arc::new(Pushed(path)) as _));
                    continue;
                }
                let (inbox, problems, reading, ctx) = (self.inbox.clone(), self.problems.clone(), self.reading.clone(), ctx.clone());
                reading.fetch_add(1, Ordering::Relaxed);
                let unread = files.clone();
                let counted = reading.clone();
                let started = std::thread::Builder::new().name("picked files".into()).spawn(move || {
                    read_batch(&purpose, files, &inbox, &problems);
                    reading.fetch_sub(1, Ordering::Relaxed);
                    ctx.request_repaint();
                });
                if let Err(e) = started {
                    counted.fetch_sub(1, Ordering::Relaxed);
                    let error = e.to_string();
                    // Nothing will read these copies now: they go, as they would have once read.
                    for (name, path) in unread {
                        remove_copy(&path);
                        self.app.session.notify(i18n::fmt("{name} wasn't opened: {error}", &[("name", &name), ("error", &error)]), true);
                    }
                }
            }
        }
    }

    /// Reads one picker's files (name, copy) into `inbox`, marked for the dialog that asked, and
    /// removes the copies: their bytes are all that was wanted of them.
    fn read_batch(purpose: &str, files: Vec<(String, PathBuf)>, inbox: &Inbox, problems: &Problems) {
        let mark = mark(purpose);
        let say = |problem| problems.lock().unwrap_or_else(|e| e.into_inner()).push(problem);
        let mut read = Vec::new();
        // The bytes read so far, of this batch.
        let mut held = 0u64;
        for (name, path) in files {
            let shortcut =
                mark == rustgis_ui_egui::SHORTCUT_PICK || (mark.is_empty() && name.to_ascii_lowercase().ends_with(".rgisshortcuts"));
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if shortcut && size > rustgis_ui_egui::SHORTCUT_FILE_MAX_BYTES as u64 {
                say(Problem::ShortcutTooLarge { name });
            } else if held.saturating_add(size) > rustgis_engine::data::io::MAX_BYTES {
                // Refused before it is read: no more than the cap is held in memory at once, in
                // one file or in the files picked together.
                say(Problem::TooLarge { name, together: size <= rustgis_engine::data::io::MAX_BYTES });
            } else {
                match std::fs::read(&path) {
                    Ok(bytes) => {
                        held += bytes.len() as u64;
                        read.push((name, bytes));
                    }
                    Err(e) => say(Problem::Unread { name, error: e.to_string() }),
                }
            }
            remove_copy(&path);
        }
        let (read, found) = rustgis_engine::data::io::gather_shapefiles(read);
        for text in found {
            say(Problem::Shapefile(text));
        }
        inbox.lock().unwrap_or_else(|e| e.into_inner()).extend(read.into_iter().map(|(name, bytes)| (format!("{mark}{name}"), bytes)));
    }

    /// Removes a picked file's copy, and its folder once that is empty.
    fn remove_copy(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        if let Some(folder) = path.parent() {
            let _ = std::fs::remove_dir(folder);
        }
    }

    /// The room the system's bars take at the window's edges (the status bar, the navigation bar,
    /// a camera cutout), in points: egui keeps every panel out of it. `None` when the phone
    /// can't say, which leaves egui's last answer in place.
    fn insets(phone: Option<&Phone>, pixels_per_point: f32) -> Option<egui::SafeAreaInsets> {
        let [left, top, right, bottom] = phone?.insets()?.map(|edge| edge.max(0.0) / pixels_per_point.max(0.1));
        Some(egui::SafeAreaInsets(egui::epaint::MarginF32 { left, top, right, bottom }))
    }

    /// What starts the name of a file from the picker for `purpose` in the inbox, as the browser's
    /// shell marks them: the front end sends a marked file to the dialog that asked for it.
    fn mark(purpose: &str) -> &'static str {
        match purpose {
            "shortcuts" => rustgis_ui_egui::SHORTCUT_PICK,
            "text" => rustgis_ui_egui::DELIMITED_PICK,
            "shapefile" => rustgis_ui_egui::SHAPEFILE_PICK,
            p if p.starts_with(rustgis_ui_egui::TOOL_FILE_PURPOSE) => rustgis_ui_egui::TOOL_FILE_PICK,
            _ => "",
        }
    }

    /// Gives the front end the phone's file picker, the way the browser's shell gives its own:
    /// Open and Add Data ask through `open_async`, and what is saved or exported is offered
    /// through `download`.
    fn use_picker(app: &mut RustGisApp, phone: &Rc<Phone>, inbox: &Inbox, refusals: &Refusals) {
        let (picker, refused) = (phone.clone(), refusals.clone());
        app.services.open_async = Some(Box::new(move |purpose: &str| {
            log::info!("asking for files: {purpose}");
            if let Err(e) = picker.pick_files(purpose) {
                log::warn!("{e}");
                refused.borrow_mut().push(e);
            }
        }));
        let saver = phone.clone();
        app.services.download = Some(Box::new(move |name: &str, bytes: Vec<u8>| saver.save_file(name, bytes)));
        app.services.inbox = Some(inbox.clone());
    }

    impl eframe::App for App {
        fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
            // The GPU map backend draws inside egui's wgpu pass; hand over the render state.
            self.app.set_render_state(frame.wgpu_render_state().cloned());
            self.app.logic(ctx);
        }

        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            // The window runs under the system's bars; the interface keeps to what they leave
            // (`insets`), over a ground of its own colour so the bars read as part of it.
            let ctx = ui.ctx().clone();
            let tokens = rustgis_ui_egui::theme::Tokens::get(&ctx);
            // The bars' icons are drawn to be read on that ground, whichever theme it is.
            if self.bars_light != Some(!tokens.dark) {
                self.bars_light = Some(!tokens.dark);
                if let Some(phone) = &self.phone {
                    phone.bars_light(!tokens.dark);
                }
            }
            ui.painter().rect_filled(ctx.viewport_rect(), 0.0, tokens.pane);
            ui.scope_builder(egui::UiBuilder::new().max_rect(ctx.content_rect()), |ui| self.app.ui(ui));
            // Something of Android's is still to arrive (a picked file, a position): look again.
            if let Some(wait) = self.poll {
                ctx.request_repaint_after(std::time::Duration::from_millis(wait));
            }
        }

        fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
            raw_input.safe_area_insets = insets(self.phone.as_deref(), ctx.pixels_per_point());
            self.take_phone_events(ctx, raw_input);
            // The pushed files open as files dropped on the desktop window do: once the window
            // runs, read off its thread with progress and Cancel, each failure said in the window.
            // Opened before the event loop, a large one would hold the activity's start.
            for path in self.pushed.drain(..) {
                raw_input.dropped_files.push(std::sync::Arc::new(Pushed(path)));
            }
            self.app.drop_stale_clicks(raw_input);
        }
    }

    /// A pushed or picked file, handed to the front end as a file dropped on the window: by its path.
    #[derive(Debug)]
    struct Pushed(std::path::PathBuf);

    impl egui::DroppedFile for Pushed {
        fn path(&self) -> &std::path::Path {
            &self.0
        }

        fn bytes(&self) -> Result<Vec<u8>, String> {
            std::fs::read(&self.0).map_err(|e| e.to_string())
        }
    }

    /// Points the app's folders (`rustgis_data::dirs`) at the app's own storage: Android has no
    /// `HOME`, so without this there are no settings, no tile cache and no recovery folder.
    /// Returns whether there is such storage.
    ///
    /// Run first in [`android_main`], before anything asks for a folder.
    fn set_folders(app: &AndroidApp) -> bool {
        match app.internal_data_path() {
            Some(data) => dirs::set_root(data),
            None => false,
        }
    }

    /// The files and folders in the app's external files folder
    /// (`/sdcard/Android/data/org.rustgis.mobile/files`, where `adb push` can put them), in name
    /// order: a way in for tests, beside the file picker. A folder or an entry that can't be read is
    /// said in the window.
    fn pushed(android: &AndroidApp, app: &mut RustGisApp) -> Vec<std::path::PathBuf> {
        let Some(dir) = android.external_data_path() else {
            app.session.warn("This device gives RustGIS no external files folder: there are no pushed files to open.");
            return Vec::new();
        };
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                app.session.notify(format!("The files in {} weren't opened: {e}", dir.display()), true);
                return Vec::new();
            }
        };
        let mut files = Vec::new();
        let mut unread = 0usize;
        let mut first = None;
        for entry in entries {
            match entry {
                // Folders too (a Zarr store is one): what the front end can't open, it says.
                Ok(entry) => files.push(entry.path()),
                Err(e) => {
                    unread += 1;
                    first.get_or_insert(e);
                }
            }
        }
        if let Some(e) = first {
            app.session.notify(format!("{unread} entries of {} couldn't be read and weren't opened: {e}", dir.display()), true);
        }
        files.sort();
        // One project: each one opened would replace the one before it, and nothing would say so.
        let mut projects = 0usize;
        files.retain(|f| {
            let project = rustgis_engine::project::open_command(&f.display().to_string()) != "layer.addData";
            projects += usize::from(project);
            !project || projects == 1
        });
        if projects > 1 {
            app.session
                .notify(format!("{} other projects in {} weren't opened: only the first project is.", projects - 1, dir.display()), false);
        }
        files
    }

    /// Android's entry point for a `NativeActivity` (`android-activity` calls it on its own thread).
    // `no_mangle` is the one unsafe attribute here: the name is what `android-activity` calls.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    fn android_main(android: AndroidApp) {
        android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("rustgis"));
        let folders = set_folders(&android);
        let mut app = RustGisApp::with_sample();
        if !folders {
            app.session.warn("This device gives RustGIS no folder of its own: settings, the tile cache and recovery copies are off.");
        }
        // As on the desktop: commands run on the window's thread must not hold it.
        app.session.in_place_wait = false;
        app.session.on_window_thread = true;
        app.reads_off_frame = true;
        if let Err(e) = app.start(&rustgis_ui_egui::Start { blank: Some(false), ..Default::default() }) {
            log::warn!("start-up: {e}");
            app.session.notify(e, true);
        }
        let (inbox, refusals) = (Inbox::default(), Refusals::default());
        let phone = match Phone::new(&android) {
            Ok(phone) => {
                let phone = Rc::new(phone);
                use_picker(&mut app, &phone, &inbox, &refusals);
                Some(phone)
            }
            Err(e) => {
                app.session.warn(i18n::fmt("{error}. Every button that asks for a file does nothing.", &[("error", &e)]));
                None
            }
        };
        // A phone gets the touch interface; its More sheet has the desktop one.
        app.chrome = Some(rustgis_ui_touch::chrome);
        let pushed = pushed(&android, &mut app);
        app.session.mark_saved();
        let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
        if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
            // Vulkan where the phone has it, OpenGL ES otherwise (older phones, the emulator).
            setup.instance_descriptor.backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
        }
        let options = eframe::NativeOptions { android_app: Some(android), wgpu_options, ..Default::default() };
        if let Err(e) = eframe::run_native(
            "RustGIS",
            options,
            Box::new(move |_cc| {
                Ok(Box::new(App {
                    app,
                    poll: None,
                    locating: false,
                    bars_light: None,
                    pushed,
                    phone,
                    inbox,
                    refusals,
                    waiting: Vec::new(),
                    problems: Problems::default(),
                    reading: Arc::default(),
                }))
            }),
        ) {
            log::error!("the app stopped: {e}");
        }
        // The activity is gone (Back, or the system destroyed it). winit's event loop can't be
        // made twice in a process, so a process left alive would answer the next launch with a
        // blank window: end it, and the next launch starts afresh. Phase 1 keeps the session
        // across this instead (`onSaveInstanceState`).
        std::process::exit(0);
    }
}
