//! RustGIS on Android: the phase 0 feasibility spike (#1902).
//!
//! The desktop front end (`rustgis-ui-egui`) in a `NativeActivity`, to find out what builds, what
//! runs and what breaks on a phone before any touch interface is designed. See `README.md` for
//! how to build and install it.
//!
//! Not here yet (phase 1): a platform trait for location, camera and file picking; preferences
//! and the keychain; autosave; a touch-first front end.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_os = "android")]
mod android {
    use eframe::egui;
    use eframe::wgpu;
    use rustgis_engine::data::dirs;
    use rustgis_ui_egui::RustGisApp;
    use winit::platform::android::activity::AndroidApp;

    /// The app: the desktop front end, with the files to open once the window runs.
    struct App {
        app: RustGisApp,
        /// The files pushed to the app's folder ([`pushed`]), until the first frame takes them.
        pushed: Vec<std::path::PathBuf>,
    }

    impl eframe::App for App {
        fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
            // The GPU map backend draws inside egui's wgpu pass; hand over the render state.
            self.app.set_render_state(frame.wgpu_render_state().cloned());
            self.app.logic(ctx);
        }

        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            self.app.ui(ui);
        }

        fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
            // The pushed files open as files dropped on the desktop window do: once the window
            // runs, read off its thread with progress and Cancel, each failure said in the window.
            // Opened before the event loop, a large one would hold the activity's start.
            for path in self.pushed.drain(..) {
                raw_input.dropped_files.push(std::sync::Arc::new(Pushed(path)));
            }
            self.app.drop_stale_clicks(raw_input);
        }
    }

    /// A pushed file, handed to the front end as a file dropped on the window: by its path.
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
    /// order: the spike's stand-in for a file picker. A folder or an entry that can't be read is
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
        // The pickers do nothing here (`rustgis_ui_egui`'s `filedialog`): said once, in the window.
        let folder = android.external_data_path().map(|d| d.display().to_string());
        app.session.warn(match folder {
            Some(folder) => format!("Android has no file picker in RustGIS yet: every button that asks for a file or folder does nothing. Files and folders in {folder} are opened at start-up."),
            None => "Android has no file picker in RustGIS yet: every button that asks for a file or folder does nothing.".to_string(),
        });
        let pushed = pushed(&android, &mut app);
        app.session.mark_saved();
        let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
        if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
            // Vulkan where the phone has it, OpenGL ES otherwise (older phones, the emulator).
            setup.instance_descriptor.backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
        }
        let options = eframe::NativeOptions { android_app: Some(android), wgpu_options, ..Default::default() };
        if let Err(e) = eframe::run_native("RustGIS", options, Box::new(move |_cc| Ok(Box::new(App { app, pushed })))) {
            log::error!("the app stopped: {e}");
        }
        // The activity is gone (Back, or the system destroyed it). winit's event loop can't be
        // made twice in a process, so a process left alive would answer the next launch with a
        // blank window: end it, and the next launch starts afresh. Phase 1 keeps the session
        // across this instead (`onSaveInstanceState`).
        std::process::exit(0);
    }
}
