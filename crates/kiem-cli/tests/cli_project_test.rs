//! Projects and todos through the CLI: resolving a project from the `.kiem`
//! marker or the directory name, and the note/todo loop an agent drives.

use predicates::prelude::*;

mod common;
use common::{json_out, kiem, kiem_in};

#[test]
fn project_current_resolves_marker_then_dirname() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    // No marker → slugified directory name.
    let from_name =
        json_out(kiem_in(data.path(), repo.path()).args(["project", "current", "--json"]));
    assert!(from_name["project"].as_str().unwrap().starts_with("proj/"));
    assert_eq!(
        from_name["onboarded"], false,
        "no marker yet → not onboarded"
    );

    // `project add` writes the marker; `current` then resolves to it.
    kiem_in(data.path(), repo.path())
        .args(["project", "add", "Demo Project"])
        .assert()
        .success();
    let from_marker =
        json_out(kiem_in(data.path(), repo.path()).args(["project", "current", "--json"]));
    assert_eq!(from_marker["project"], "proj/demo_project");
    assert_eq!(
        from_marker["onboarded"], true,
        "marker committed → onboarded"
    );
    assert!(
        repo.path().join(".kiem").is_file(),
        "marker committed to repo"
    );
    assert!(
        repo.path().join("AGENTS.md").is_file(),
        "AGENTS.md pointer written"
    );
}

#[test]
fn project_add_is_idempotent_no_duplicate_home_note() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let first =
        json_out(kiem_in(data.path(), repo.path()).args(["project", "add", "Demo", "--json"]));
    assert!(
        first["home_note"].is_string(),
        "first add creates a home note"
    );

    let second =
        json_out(kiem_in(data.path(), repo.path()).args(["project", "add", "Demo", "--json"]));
    assert!(
        second["home_note"].is_null(),
        "re-add binds without a second home note"
    );

    // Exactly one project, one note.
    let projects = json_out(kiem_in(data.path(), repo.path()).args(["project", "list", "--json"]));
    assert_eq!(projects.as_array().unwrap().len(), 1);
}

#[test]
fn agent_loop_add_note_list_todos_then_check() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    kiem_in(data.path(), repo.path())
        .args(["project", "add", "Demo"])
        .assert()
        .success();

    // Add a note with two open todos.
    let note = json_out(kiem_in(data.path(), repo.path()).args([
        "note",
        "add",
        "# Tasks\n- [ ] first\n- [ ] second",
        "--json",
    ]));
    let note_id = note["id"].as_str().unwrap().to_owned();

    // `todos` aggregates the project's open items (home note has none).
    let todos = json_out(kiem_in(data.path(), repo.path()).args(["todos", "--json"]));
    let items = todos.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["note_id"], note_id.as_str());
    assert_eq!(items[0]["index"], 0);

    // Check the first todo → it drops out of the aggregate.
    kiem(data.path())
        .args(["todo", "check", &note_id, "0"])
        .assert()
        .success();
    let after = json_out(kiem_in(data.path(), repo.path()).args(["todos", "--json"]));
    assert_eq!(after.as_array().unwrap().len(), 1);
    assert_eq!(after[0]["text"], "second");
}

#[test]
fn todo_check_accepts_multiple_stable_indices() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    kiem_in(data.path(), repo.path())
        .args(["project", "add", "Demo"])
        .assert()
        .success();

    let note = json_out(kiem_in(data.path(), repo.path()).args([
        "note",
        "add",
        "# Tasks\n- [ ] first\n- [ ] second\n- [ ] third",
        "--json",
    ]));
    let note_id = note["id"].as_str().unwrap().to_owned();

    kiem(data.path())
        .args(["todo", "check", &note_id, "0", "2"])
        .assert()
        .success();
    let after = json_out(kiem_in(data.path(), repo.path()).args(["todos", "--json"]));
    let items = after.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["index"], 1);
    assert_eq!(items[0]["text"], "second");
}

#[test]
fn note_add_does_not_duplicate_an_already_present_project_tag() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    kiem_in(data.path(), repo.path())
        .args(["project", "add", "Demo"])
        .assert()
        .success();

    // Text already carries the project tag — it must not be appended again.
    let note = json_out(kiem_in(data.path(), repo.path()).args([
        "note",
        "add",
        "# Hand tagged\n\n#proj/demo",
        "--json",
    ]));
    let id = note["id"].as_str().unwrap().to_owned();
    let shown = json_out(kiem(data.path()).args(["show", &id, "--json"]));
    let body = shown["body"].as_str().unwrap();
    assert_eq!(body.matches("#proj/demo").count(), 1, "body: {body:?}");
    assert_eq!(
        shown["tags"].as_array().unwrap(),
        &[serde_json::json!("proj/demo")]
    );
}

#[test]
fn todo_add_appends_one_item_in_a_single_command() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    kiem_in(data.path(), repo.path())
        .args(["project", "add", "Demo"])
        .assert()
        .success();
    let note = json_out(kiem_in(data.path(), repo.path()).args([
        "note",
        "add",
        "# Tasks\n- [ ] first",
        "--json",
    ]));
    let id = note["id"].as_str().unwrap().to_owned();

    // One command, no whole-body rewrite — the new item is addressable as index 1.
    kiem(data.path())
        .args(["todo", "add", &id, "second"])
        .assert()
        .success();
    let todos = json_out(kiem_in(data.path(), repo.path()).args(["todos", "--json"]));
    let items = todos.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[1]["text"], "second");

    // Empty text is rejected rather than writing a blank checkbox.
    kiem(data.path())
        .args(["todo", "add", &id, "   "])
        .assert()
        .failure();
}

#[test]
fn edit_lines_targets_a_line_and_honors_expect_version() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    kiem_in(data.path(), repo.path())
        .args(["project", "add", "Demo"])
        .assert()
        .success();
    let note = json_out(kiem_in(data.path(), repo.path()).args([
        "note",
        "add",
        "# T\n- [ ] a ☕\n- [ ] b",
        "--json",
    ]));
    let id = note["id"].as_str().unwrap().to_owned();
    let shown = json_out(kiem(data.path()).args(["show", &id, "--json"]));
    let version = shown["version"].as_str().unwrap().to_owned();

    // Replace line 2 with the correct version; the multibyte line stays intact.
    kiem(data.path())
        .args([
            "edit-lines",
            &id,
            "2",
            "2",
            "--text",
            "- [x] a ☕",
            "--expect",
            &version,
        ])
        .assert()
        .success();
    // `note add` auto-appended `#proj/demo`; only line 2 changed.
    let after = json_out(kiem(data.path()).args(["show", &id, "--json"]));
    assert_eq!(after["body"], "# T\n- [x] a ☕\n- [ ] b\n\n#proj/demo");

    // The stale version is now rejected.
    kiem(data.path())
        .args([
            "edit-lines",
            &id,
            "3",
            "3",
            "--text",
            "- [x] b",
            "--expect",
            &version,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("changed since you read it"));
}

#[test]
fn todo_check_bad_index_fails() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    kiem_in(data.path(), repo.path())
        .args(["project", "add", "Demo"])
        .assert()
        .success();
    let note = json_out(kiem_in(data.path(), repo.path()).args([
        "note",
        "add",
        "# T\n- [ ] only",
        "--json",
    ]));
    let note_id = note["id"].as_str().unwrap().to_owned();
    kiem(data.path())
        .args(["todo", "check", &note_id, "9"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("out of range"));
}

#[test]
fn note_add_type_and_notes_type_filter() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    kiem_in(data.path(), repo.path())
        .args(["project", "add", "Demo"])
        .assert()
        .success();

    let plan = json_out(
        kiem_in(data.path(), repo.path())
            .args(["note", "add", "# Plan", "--type", "plan", "--json"]),
    );
    assert_eq!(plan["note_type"], "plan");
    kiem_in(data.path(), repo.path())
        .args(["note", "add", "# Just a note"])
        .assert()
        .success();

    // --type filters; default add stays "note".
    let plans =
        json_out(kiem_in(data.path(), repo.path()).args(["notes", "--type", "plan", "--json"]));
    assert_eq!(plans.as_array().unwrap().len(), 1);
    assert_eq!(plans[0]["title"], "Plan");
    // Home note + plain note are the two "note"-typed entries.
    let notes =
        json_out(kiem_in(data.path(), repo.path()).args(["notes", "--type", "note", "--json"]));
    assert_eq!(notes.as_array().unwrap().len(), 2);
}
