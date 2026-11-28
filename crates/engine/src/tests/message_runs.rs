//! The Messages pane groups a command's messages by its run and can undo back to before it.

use super::*;

#[test]
fn a_commands_messages_remember_its_run_and_undo_reaches_back_past_it() {
    let mut s = world();
    s.run("layer.addDataText", &json!({"fileName": "records.csv", "text": "n\n1\n2\n"})).unwrap();
    s.run("edit.enable", &json!({"on": true})).unwrap();
    let last_run = |s: &Session| -> rustgis_data::diagnostics::Run {
        s.diagnostics.records().into_iter().rev().find(|r| r.loss.is_some()).and_then(|r| r.run).expect("a coded message from a run")
    };
    s.run("edit.calculateField", &json!({"table": "records", "field": "n", "expression": "n + 1.5"})).unwrap();
    let first = last_run(&s);
    assert_eq!(first.target.as_deref(), Some("records"));
    assert_eq!(first.command, "edit.calculateField");
    let after = first.after.expect("finished");
    assert_ne!(after, first.before, "it changed the project");
    assert_eq!(s.undo_steps_back_past(first.before, after), Some(1), "one step back past it");

    s.run("edit.calculateField", &json!({"table": "records", "field": "n", "expression": "n + 0.5"})).unwrap();
    let later = last_run(&s);
    assert_ne!(later.id, first.id, "each run is its own");
    assert_eq!(s.undo_steps_back_past(first.before, after), Some(2), "back past the later run too");
    assert_eq!(s.undo_steps_back_past(later.before, later.after.unwrap()), Some(1));

    // Both undone, then another edit from the state before the first: the first run's change is
    // gone from the history, so its message offers nothing, not the new edit.
    s.run("edit.undo", &json!({})).unwrap();
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(s.state(), first.before);
    s.run("edit.calculateField", &json!({"table": "records", "field": "n", "expression": "n * 2"})).unwrap();
    assert_eq!(s.undo_steps_back_past(first.before, after), None);
    // A run whose step joined an earlier one's (a held key nudging an element) has no step of its
    // own: Undo would land before the earlier command, so its message offers nothing.
    s.merge_undo("nudge");
    s.run("edit.calculateField", &json!({"table": "records", "field": "n", "expression": "n + 1.5"})).unwrap();
    s.merge_undo("nudge");
    s.run("edit.calculateField", &json!({"table": "records", "field": "n", "expression": "n + 2.5"})).unwrap();
    let joined = last_run(&s);
    assert_eq!(s.undo_steps_back_past(joined.before, joined.after.unwrap()), None);
    // A message no command said has no run.
    s.notify("said by the app", false);
    assert!(s.diagnostics.records().last().is_some_and(|r| r.run.is_none()));
}

#[test]
fn a_runs_target_masks_a_login_in_use() {
    let mut s = Session::new(Project::default());
    let id = s
        .run(
            "connection.new",
            &json!({"kind": "wms", "name": "Run", "address": "https://run-target.example.invalid", "auth": "header", "keyName": "X-Key"}),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let secret = "RGSYNTH_run_target_key";
    s.run("connection.login", &json!({"id": id, "apiKey": secret})).unwrap();
    // A layer named with the key: the command fails, and its message keeps the run's target.
    let _ = s.run("layer.zoomTo", &json!({"layer": format!("roads {secret}")}));
    let run = s.diagnostics.records().into_iter().rev().find_map(|r| r.run).expect("the failure's run");
    let target = run.target.expect("a target");
    assert!(!target.contains(secret), "{target}");
    assert!(target.starts_with("roads "), "{target}");
}

#[test]
fn a_bug_in_a_command_is_said_as_one_with_its_details_kept() {
    rustgis_data::incident::install();
    let mut s = Session::new(Project::default());
    s.registry.commands.push(crate::CommandSpec::new("test.bug", "Bug", "Test", |_, _| panic!("the test's bug")));
    let error = s.run("test.bug", &json!({})).unwrap_err();
    assert!(error.contains("failed unexpectedly, a bug in RustGIS (incident "), "{error}");
    let id = rustgis_data::incident::named_in(&error).expect("a reference").to_string();
    // Messages keeps what a report needs: the reference, the panic's message and its place.
    let record = s.diagnostics.records().into_iter().rev().find(|r| r.message.contains(&id)).expect("the failure in Messages");
    assert!(record.details.contains(&format!("Incident {id}")), "{}", record.details);
    assert!(record.details.contains("the test's bug") && record.details.contains("message_runs.rs"), "{}", record.details);
    // The next command runs as usual.
    s.run("map.zoomIn", &json!({})).unwrap();
}

#[test]
fn a_command_run_is_told_as_it_starts_and_ends() {
    let mut s = world();
    let told = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let heard = told.clone();
    s.on_run = Some(Box::new(move |run| heard.lock().unwrap().push(run.map(|r| (r.command.clone(), r.target.clone())))));
    s.run("layer.zoomTo", &json!({"layer": "Cities"})).unwrap();
    assert_eq!(*told.lock().unwrap(), [Some(("layer.zoomTo".to_string(), Some("Cities".to_string()))), None]);
}
