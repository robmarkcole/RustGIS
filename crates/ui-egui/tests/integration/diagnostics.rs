//! Troubleshooting history survives pane closure; filters and copying use the same entries.
use egui_kittest::kittest::Queryable;
use rustgis_engine::{Project, Session};
use rustgis_ui_egui::{RightPane, RustGisApp};
use serde_json::json;

#[test]
fn diagnostics_preserves_closed_history_filters_copies_and_clears() {
    let mut app = RustGisApp::new(Session::new(Project::default()));
    app.session.map_mut().tiles.clear();
    app.ui.tab = "View".into();
    app.session.diagnostics.clear();
    assert!(app.run("layer.addData", json!({"path":"/diagnostics-missing/absent-first.geojson"})).is_err());
    assert!(app.run("layer.addData", json!({"path":"/diagnostics-missing/absent-second.geojson"})).is_err());
    let ui_error = app.run("ui.addPicture", json!({})).unwrap_err();
    app.session.notify("Finished loading the local sample", false);
    app.session.warn("Skipped an unsupported symbol");
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1366.0, 900.0)).with_step_dt(0.1).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    // Let transient toasts expire without making individual pointer clicks long presses.
    h.run_steps(70);
    // The latest brief notice itself opens the history, not just the permanent status button.
    h.get_by_label("Skipped an unsupported symbol").click();
    h.run_steps(3);
    assert_eq!(h.state().ui.right, Some(RightPane::DiagnosticLog));
    h.get_by_label("Finished loading the local sample");
    h.get_by_label("Errors (3)").click();
    h.run_steps(3);
    assert!(h.query_by_label("Finished loading the local sample").is_none());
    h.get_by_label("Copy diagnostics").click();
    h.step();
    let copied = h
        .output()
        .platform_output
        .commands
        .iter()
        .find_map(|command| match command {
            egui::OutputCommand::CopyText(text) => Some(text),
            _ => None,
        })
        .expect("filtered diagnostics on the clipboard");
    assert!(copied.contains("absent-first") && copied.contains("absent-second"), "{copied}");
    assert!(copied.contains(&ui_error), "{copied}");
    assert!(!copied.contains("Finished loading") && !copied.contains("unsupported symbol"), "{copied}");
    h.state_mut().run("ui.pane.diagnosticLog", json!({"close":true})).unwrap();
    h.state_mut().session.notify("New message while closed https://user:secret@example.org/?token=private", false);
    h.state_mut().run("ui.pane.diagnosticLog", json!({})).unwrap();
    h.run_steps(3);
    h.get_by_label("Info (2)").click();
    h.run_steps(3);
    h.get_by_label("Finished loading the local sample");
    h.get_by_label("Copy diagnostics").click();
    h.step();
    let copied = h
        .output()
        .platform_output
        .commands
        .iter()
        .find_map(|command| match command {
            egui::OutputCommand::CopyText(text) => Some(text),
            _ => None,
        })
        .unwrap();
    assert!(copied.contains("New message while closed") && copied.contains("example.org"), "{copied}");
    assert!(!copied.contains("secret") && !copied.contains("private"), "{copied}");
    h.get_by_label("Warnings (1)").click();
    h.run_steps(3);
    h.get_by_label("Skipped an unsupported symbol");
    assert!(h.query_by_label("Finished loading the local sample").is_none());
    h.get_by_label("Clear").click();
    h.run_steps(3);
    assert!(h.state().session.diagnostics.records().is_empty());
    h.get_by_label("No diagnostic entries match this filter.");
    h.state_mut().run("ui.pane.diagnostics", json!({})).unwrap();
    h.run_steps(3);
    assert_eq!(h.state().ui.right, Some(RightPane::Diagnostics));
    h.get_by_label("Map draw");
}

/// Report a Problem shows the report before anything leaves the app; Copy copies it, and Report
/// on GitHub copies it and opens the bug report form, sending nothing itself.
#[test]
fn report_a_problem_shows_copies_and_opens_the_bug_report_form() {
    let mut app = RustGisApp::new(Session::new(Project::default()));
    app.session.map_mut().tiles.clear();
    app.session.diagnostics.clear();
    app.last_run = rustgis_engine::problem_report::LastRun::Left("RustGIS didn't close normally last time, between commands.".into());
    assert!(app.run("layer.addData", json!({"path":"/report-missing/absent.geojson"})).is_err());
    app.run("ui.reportProblem", json!({})).unwrap();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1366.0, 900.0)).with_step_dt(0.1).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    h.run_steps(3);
    let text = h.state().ui.problem_report.as_ref().map(|w| w.text.clone()).expect("the window is open");
    assert!(text.contains("Last run: RustGIS didn't close normally"), "{text}");
    assert!(text.contains("layer.addData {\"path\":\"/report-missing/absent.geojson\"}"), "{text}");
    let output = |h: &egui_kittest::Harness<'_, RustGisApp>| h.output().platform_output.commands.clone();
    h.get_by_label("Copy").click();
    h.step();
    assert!(output(&h).iter().any(|c| matches!(c, egui::OutputCommand::CopyText(t) if *t == text)), "{:?}", output(&h));
    h.get_by_label("Report on GitHub…").click();
    h.step();
    let opened = output(&h);
    assert!(
        opened.iter().any(|c| matches!(c, egui::OutputCommand::OpenUrl(u) if u.url.starts_with(rustgis_engine::problem_report::NEW_ISSUE))),
        "{opened:?}"
    );
    // The report stays on the clipboard, out of the address.
    assert!(!opened.iter().any(|c| matches!(c, egui::OutputCommand::OpenUrl(u) if u.url.contains("report-missing"))), "{opened:?}");
    assert!(opened.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t.contains("report-missing"))), "{opened:?}");
    h.get_by_label_contains("Opened the bug report form");
}
