//! The system's file dialogs: `rfd` on the desktop, nothing on Android.
//!
//! `rfd` has no Android backend. The mobile app's own picker (the Storage Access Framework) is
//! asynchronous and reaches the front end through `Services::open_async`, as the browser's does,
//! so it can't stand in for these calls, which return a path at once. What still comes here on
//! Android (choosing a folder, a few pickers `open_async` doesn't cover) answers as if cancelled
//! and says so: [`take_refused`] gives the front end what to tell the user.

#[cfg(not(target_os = "android"))]
pub use rfd::FileDialog;

#[cfg(target_os = "android")]
pub use android::{FileDialog, take_refused};

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

    /// What the user asked for and didn't get, until the next frame says it.
    static REFUSED: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());

    /// Cancelled, and kept to be said in the window: there is no such picker on Android.
    fn none(what: &'static str) -> Option<PathBuf> {
        log::warn!("no file dialog on Android: can't {what}");
        REFUSED.lock().unwrap_or_else(|e| e.into_inner()).push(what);
        None
    }

    /// The pickers asked for since the last call that Android doesn't have (`"open a file"`,
    /// `"choose a folder"`, `"save a file"`).
    pub fn take_refused() -> Vec<&'static str> {
        std::mem::take(&mut *REFUSED.lock().unwrap_or_else(|e| e.into_inner()))
    }
}
