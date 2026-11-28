//! What a bug report needs from this session, as one text the user reads before sharing it: the
//! RustGIS and computer it ran on, the bugs it met ([`rustgis_data::incident`]), the last commands
//! as Script pane lines to run again, and the Messages history. Logins and secrets are taken out:
//! the free text by [`Session::redacted`], a command's parameters as the session log kept them
//! (secrets already out) with addresses and active logins scrubbed, and Messages as the history
//! kept them (redacted as they were said). File paths and layer names stay, as the report needs
//! them, and the user sees them before anything is copied.

use std::fmt::Write;

use rustgis_data::diagnostics::Filter;

use crate::Session;
use crate::project_log::LogEntry;

/// The most commands the report gives, the newest.
pub const COMMANDS: usize = 50;

/// What only the front end knows.
#[derive(Clone, Debug, Default)]
pub struct About {
    /// The front end: `desktop`, `browser`, `phone`.
    pub front_end: String,
    /// The graphics the map is drawn with (backend, adapter, driver), when it says.
    pub graphics: Option<String>,
    /// What the last run of the app left.
    pub last_run: LastRun,
}

/// How the app's last run ended, as its shell knows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LastRun {
    /// The shell doesn't keep track (the browser, the phone), or couldn't this time.
    #[default]
    NotReported,
    Closed,
    /// It didn't close normally, or met an internal error: the shell's notice, which says what
    /// it was running.
    Left(String),
}

impl Session {
    /// The report, logins and secrets taken out. Each section says when it is empty, so a reader
    /// tells "none" from "left out".
    pub fn problem_report(&self, about: &About) -> String {
        let mut out = self.report_head(about);
        out.push_str(&self.commands_section());
        let messages = self.diagnostics.report(Filter::All);
        // The history's own heading, under this one.
        let messages = messages.split_once('\n').map_or(messages.as_str(), |(_, rest)| rest);
        let _ = writeln!(out, "\n## Messages ({})", self.diagnostics.records().len());
        out.push_str(messages);
        out
    }

    /// The app, the computer, the last run and the incidents. Each part is redacted on its own:
    /// the redaction bounds what it is given, and the whole of them could pass that bound, cutting
    /// the newest incidents or putting all of it out.
    fn report_head(&self, about: &About) -> String {
        let redacted = |text: &str| Self::redacted(text).into_string();
        let mut out = String::from("RustGIS diagnostics\n");
        let _ = writeln!(out, "Version: {}", env!("CARGO_PKG_VERSION"));
        let _ = writeln!(out, "System: {} {}", std::env::consts::OS, std::env::consts::ARCH);
        if !about.front_end.is_empty() {
            let _ = writeln!(out, "Front end: {}", about.front_end);
        }
        let graphics = about.graphics.as_deref().map_or_else(|| "not reported".to_string(), redacted);
        let _ = writeln!(out, "Graphics: {graphics}");
        let _ = writeln!(out, "Threads: {} of {} cores", rustgis_data::threads::max(), rustgis_data::threads::cores());
        let last_run = match &about.last_run {
            LastRun::NotReported => "not reported".to_string(),
            LastRun::Closed => "closed normally".to_string(),
            LastRun::Left(notice) => redacted(notice),
        };
        let _ = writeln!(out, "Last run: {last_run}");

        let incidents = rustgis_data::incident::kept();
        let _ = writeln!(out, "\n## Incidents ({})", incidents.len());
        if incidents.is_empty() {
            out.push_str("None this session.\n");
        }
        for incident in &incidents {
            // The reference and version as they are (the reference is six hexadecimal digits), so a
            // message past the redaction's bound, which puts all of it out, doesn't take them too.
            let _ = writeln!(out, "\nIncident {} in RustGIS {}", incident.id, env!("CARGO_PKG_VERSION"));
            let _ = writeln!(out, "{}", redacted(&incident.message));
            if let Some(at) = &incident.location {
                let _ = writeln!(out, "at {}", redacted(at));
            }
        }
        out
    }

    /// The last [`COMMANDS`] commands as Script pane lines, oldest first, each after a comment
    /// saying when it ran and how it ended: one that failed says its error, and one the log kept
    /// less of (cut to fit, secrets taken out) or that a script can't run says so, as it
    /// wouldn't run again as it ran.
    fn commands_section(&self) -> String {
        let (entries, _, total) = self.log.select(|_| true, COMMANDS);
        let mut out = format!("\n## Last commands ({} of {total}), as a script\n", entries.len());
        if !self.log.on() {
            out.push_str("# The session log is turned off: commands since then aren't listed.\n");
        }
        if let Some(dropped) = self.log.dropped_said() {
            let _ = writeln!(out, "# {dropped}.");
        }
        if entries.is_empty() {
            out.push_str("None this session.\n");
        }
        for e in &entries {
            let ended = match &e.error {
                Some(error) => format!("failed: {}", error.replace('\n', " ")),
                None if e.ok => "ok".to_string(),
                None => "failed".to_string(),
            };
            let mut said = vec![format!("{} UTC, {} ms, {ended}", e.time_utc(), e.millis)];
            // Its parameters as the log kept them, secrets out, with addresses and logins in use
            // scrubbed now, value by value before they are written as JSON (whose escapes would
            // hide a login with a quote in it); a redaction made for prose would take the JSON
            // out with them. A scrub that changed them changed what it runs.
            let scrubbed = LogEntry { params: scrubbed_values(&e.params), ..e.clone() };
            let line = scrubbed.script_line();
            if e.partial || scrubbed.params != e.params {
                said.push("its parameters were cut or had secrets taken out, so it may not run as it ran".into());
            }
            let scriptable = self.registry.get(&e.command).is_some() && !crate::cmd::workflows::refused_in_script(&e.command);
            if !scriptable {
                said.push("a script can't run it".into());
            }
            let said = Self::redacted(&said.join("; ")).into_string();
            let _ = writeln!(out, "# {}", said.replace('\n', " "));
            if scriptable {
                let _ = writeln!(out, "{line}");
            } else {
                let _ = writeln!(out, "# {line}");
            }
        }
        out
    }
}

/// `params` with each text in it, keys too, scrubbed of addresses' logins and the logins in use; a
/// number too, as a login can be one, given as text when the scrub changed it.
fn scrubbed_values(params: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match params {
        Value::String(s) => Value::String(crate::durable::generated_text(s)),
        Value::Number(n) => {
            let text = n.to_string();
            let scrubbed = crate::durable::generated_text(&text);
            if scrubbed == text { params.clone() } else { Value::String(scrubbed) }
        }
        Value::Array(items) => Value::Array(items.iter().map(scrubbed_values).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (crate::durable::generated_text(k), scrubbed_values(v))).collect()),
        other => other.clone(),
    }
}

/// Where a new bug report starts: the repository's bug report form.
pub const NEW_ISSUE: &str = "https://github.com/opengeos/RustGIS/issues/new";

/// A new bug report on GitHub, its form filled in with the version and the system and graphics
/// (`about`), which go to GitHub as the form opens; its logs field asks for the report to be
/// pasted, so the report itself goes only once the user pastes it and submits.
pub fn issue_address(about: &About) -> String {
    let mut system = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    if !about.front_end.is_empty() {
        let _ = write!(system, ", {}", about.front_end);
    }
    if let Some(graphics) = &about.graphics {
        // Redacted as the report's own line is.
        let _ = write!(system, ", {}", Session::redacted(graphics).as_str());
    }
    let logs = "Paste the diagnostics from Help › Report a Problem here: Report on GitHub put them on the clipboard.";
    let fields = [("template", "bug_report.yml"), ("version", env!("CARGO_PKG_VERSION")), ("os", system.as_str()), ("screenshots", logs)];
    let query: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={}", crate::cmd::webdata::percent_encode(v))).collect();
    format!("{NEW_ISSUE}?{}", query.join("&"))
}

/// `report` as it is pasted into the form: in a fence longer than any run of backticks in it, so
/// none of it ends the fence and reads as the form's Markdown.
pub fn fenced(report: &str) -> String {
    let mut fence = "```".to_string();
    while report.contains(fence.as_str()) {
        fence.push('`');
    }
    format!("{fence}text\n{report}\n{fence}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::Project;

    fn entry(command: &str, params: serde_json::Value) -> LogEntry {
        LogEntry { command: command.into(), params, ok: true, ..Default::default() }
    }

    #[test]
    fn the_report_names_the_app_the_bugs_the_commands_and_the_messages() {
        rustgis_data::incident::install();
        let mut s = Session::new(Project::default());
        s.log.clear();
        s.diagnostics.clear();
        let payload = std::panic::catch_unwind(|| panic!("a report's own bug")).unwrap_err();
        let incident = rustgis_data::incident::caught(payload.as_ref());
        s.log.push(entry("map.new", json!({"name": "Parcels"})));
        s.log.push(LogEntry { error: Some("no such file".into()), ok: false, ..entry("layer.addData", json!({"path": "/x/a.shp"})) });
        s.log.push(LogEntry { partial: true, ..entry("map.new", json!({"name": "S…"})) });
        s.log.push(entry("script.run", json!({"script": "map.new"})));
        s.log.push(entry("layer.addData", json!({"path": "https://user:secret@example.org/a.geojson?token=private"})));
        s.notify("Read 3 features https://user:secret@example.org/?token=private", false);
        let about = About { front_end: "desktop".into(), graphics: Some("Vulkan, Test GPU".into()), last_run: LastRun::Closed };
        let report = s.problem_report(&about);
        assert!(report.contains(&format!("Version: {}", env!("CARGO_PKG_VERSION"))), "{report}");
        assert!(report.contains("Graphics: Vulkan, Test GPU") && report.contains("Last run: closed normally"), "{report}");
        assert!(report.contains(&format!("Incident {}", incident.id)) && report.contains("a report's own bug"), "{report}");
        // The commands run again as a script; the failed one says why, the cut one says so.
        assert!(report.contains("\nmap.new {\"name\":\"Parcels\"}\n"), "{report}");
        assert!(report.contains("failed: no such file\nlayer.addData {\"path\":\"/x/a.shp\"}\n"), "{report}");
        assert!(report.contains("may not run as it ran\nmap.new {\"name\":\"S…\"}\n"), "{report}");
        assert!(report.contains("a script can't run it\n# script.run"), "{report}");
        assert!(report.contains("Read 3 features") && report.contains("example.org"), "{report}");
        // A login in an address is taken out of a command's parameters and of a message alike.
        assert!(report.contains("layer.addData {\"path\":\"https://example.org/"), "{report}");
        // Scrubbed now, it no longer runs as it ran, and says so.
        assert!(report.contains("may not run as it ran\nlayer.addData {\"path\":\"https://example.org/"), "{report}");
        assert!(!report.contains("user:secret") && !report.contains("token=private"), "{report}");
    }

    #[test]
    fn the_issue_address_fills_the_form_but_leaves_the_report_out() {
        let about = About { front_end: "desktop".into(), graphics: Some("Vulkan & co".into()), last_run: LastRun::NotReported };
        let address = issue_address(&about);
        assert!(address.starts_with(NEW_ISSUE) && address.contains("template=bug_report.yml"), "{address}");
        // Values are escaped: an `&` doesn't start another field.
        assert!(address.contains("desktop%2C%20Vulkan%20%26%20co") && address.contains("Paste%20the%20diagnostics"), "{address}");
        // Backticks in the report don't end its fence.
        assert_eq!(fenced("a ``` b"), "````text\na ``` b\n````");
    }

    /// Incidents past the redaction's bound together are each kept whole, and the lines above
    /// them too.
    #[test]
    fn many_long_incidents_keep_the_head_and_each_other() {
        rustgis_data::incident::install();
        let s = Session::new(Project::default());
        let ids: Vec<String> = (0..6)
            .map(|i| {
                // The last past the redaction's whole bound, which puts its message out.
                let long = if i == 5 { 20_000 } else { 4000 };
                let payload = std::panic::catch_unwind(move || panic!("long bug {i} {}", "x".repeat(long))).unwrap_err();
                rustgis_data::incident::caught(payload.as_ref()).id
            })
            .collect();
        let report = s.problem_report(&About::default());
        assert!(report.starts_with("RustGIS diagnostics\nVersion: "), "{report}");
        for id in &ids {
            assert!(report.contains(&format!("Incident {id}")), "{id}");
        }
    }

    #[test]
    fn an_empty_session_says_each_section_is_empty() {
        let mut s = Session::new(Project::default());
        s.log.clear();
        s.diagnostics.clear();
        let report = s.problem_report(&About::default());
        assert!(report.contains("Last run: not reported"), "{report}");
        let report = s.problem_report(&About { last_run: LastRun::Left("didn't close".into()), ..Default::default() });
        assert!(report.contains("## Last commands (0 of 0), as a script\nNone this session."), "{report}");
        assert!(report.contains("Graphics: not reported") && report.contains("Last run: didn't close"), "{report}");
        assert!(report.contains("## Messages (0)"), "{report}");
    }
}
