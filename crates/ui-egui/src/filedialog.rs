//! The system's file dialogs: `rfd` on the desktop, nothing yet on Android.
//!
//! `rfd` has no Android backend. Until the mobile app has a picker of its own (the Storage
//! Access Framework, #1902 phase 1), every dialog there answers as if cancelled and logs that it
//! did, so the front end builds and runs with the pickers inert.

#[cfg(not(target_os = "android"))]
pub use rfd::FileDialog;

#[cfg(target_os = "android")]
pub use android::FileDialog;

#[cfg(target_os = "android")]
mod android {
    use std::path::PathBuf;

    /// `crate::filedialog::FileDialog`'s builder, as far as this crate uses it. Every dialog is cancelled.
    #[derive(Default)]
    pub struct FileDialog;

    impl FileDialog {
        pub fn new() -> Self {
            Self
        }

        pub fn add_filter(self, _name: impl Into<String>, _extensions: &[impl ToString]) -> Self {
            self
        }

        pub fn set_title(self, _title: impl Into<String>) -> Self {
            self
        }

        pub fn set_file_name(self, _name: impl Into<String>) -> Self {
            self
        }

        pub fn pick_file(self) -> Option<PathBuf> {
            none("open a file")
        }

        pub fn pick_folder(self) -> Option<PathBuf> {
            none("choose a folder")
        }

        pub fn save_file(self) -> Option<PathBuf> {
            none("save a file")
        }
    }

    /// Cancelled, and said in the log: there is no picker on Android yet.
    fn none(what: &str) -> Option<PathBuf> {
        log::warn!("no file dialog on Android yet: can't {what}");
        None
    }
}
