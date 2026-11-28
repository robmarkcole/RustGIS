//! Help › Report a Problem: the session's diagnostics ([`Session::problem_report`]) shown in full,
//! for the user to read and edit before anything leaves the app, with Copy, and Report on GitHub,
//! which copies them and opens the bug report form with the version and system filled in. The
//! diagnostics stay on the clipboard: they reach GitHub only when the user pastes and submits.
//!
//! [`Session::problem_report`]: rustgis_engine::Session::problem_report

use egui::{Align2, RichText};
use rustgis_engine::problem_report::{About, fenced, issue_address};

use crate::RustGisApp;
use crate::theme::Tokens;

/// The open window: the report as it was when opened (so it doesn't change while read), as the
/// user edited it, and what its last button did.
pub struct Window {
    pub text: String,
    about: About,
    said: Option<String>,
}

impl RustGisApp {
    /// What the report says only the front end knows: which one it is, the graphics the map is
    /// drawn with, and what the last run left ([`RustGisApp::last_run`]).
    pub fn problem_about(&self) -> About {
        let front_end = if cfg!(target_arch = "wasm32") {
            "browser"
        } else if cfg!(any(target_os = "android", target_os = "ios")) {
            "phone"
        } else {
            "desktop"
        };
        let graphics = self.map.render_state.as_ref().map(|rs| {
            let info = rs.adapter.get_info();
            let mut said = format!("{:?}, {} ({:?})", info.backend, info.name, info.device_type);
            if !info.driver.is_empty() {
                said.push_str(&format!(", driver {} {}", info.driver, info.driver_info));
            }
            said.trim_end().to_string()
        });
        About { front_end: front_end.into(), graphics, last_run: self.last_run.clone() }
    }

    /// Opens the window with the report as it is now.
    pub(crate) fn open_problem_report(&mut self) {
        let about = self.problem_about();
        let text = self.session.problem_report(&about);
        self.ui.problem_report = Some(Window { text, about, said: None });
    }
}

pub(crate) fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(window) = app.ui.problem_report.as_mut() else { return };
    let t = Tokens::get(ctx);
    let mut open = true;
    let mut copy = None;
    let mut visit = None;
    egui::Window::new(tl!("Report a Problem"))
        .open(&mut open)
        .collapsible(false)
        .default_size([640.0, 520.0])
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            crate::i18n::paragraph(
                ui,
                "These diagnostics help someone find what went wrong: this RustGIS and computer, the bugs met, the last commands as a script that runs them again, and Messages. Logins and secrets RustGIS recognises are taken out, but not every password can be told from other text, and file paths and layer names stay. Read them first, and delete anything you don't want to share: Copy and Report on GitHub use the text as you leave it.",
                crate::theme::regular(12.0),
                t.text,
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button(tl!("Copy")).on_hover_text(tl!("Copy the diagnostics to paste into a bug report")).clicked() {
                    copy = Some(window.text.clone());
                    window.said = Some(tl!("Copied.").to_string());
                }
                let report = ui
                    .button(tl!("Report on GitHub…"))
                    .on_hover_text(tl!("Copy the diagnostics and open a new bug report on GitHub with the version and system filled in. Paste the diagnostics into it; nothing is posted until you submit it."));
                if report.clicked() {
                    copy = Some(fenced(&window.text));
                    visit = Some(issue_address(&window.about));
                    window.said = Some(tl!("Opened the bug report form. Paste the diagnostics from the clipboard into its logs field.").to_string());
                }
                if let Some(said) = &window.said {
                    ui.label(RichText::new(said.as_str()).small().color(t.text_dim));
                }
            });
            ui.separator();
            let label = ui.label(RichText::new(tl!("Diagnostics")).small().color(t.text_dim));
            egui::ScrollArea::vertical().id_salt("problem_report_text").auto_shrink([false, false]).show(ui, |ui| {
                // Editable: what the user takes out isn't copied.
                ui.add(egui::TextEdit::multiline(&mut window.text).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY)).labelled_by(label.id);
            });
        });
    if let Some(text) = copy {
        ctx.copy_text(text);
    }
    if let Some(address) = visit {
        app.ui.open_url = Some(address);
    }
    if !open {
        app.ui.problem_report = None;
    }
}
