//! End-to-end CLI tests against a temp data directory.

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

fn kiem(data_dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("kiem").unwrap();
    cmd.arg("--data-dir").arg(data_dir);
    cmd
}

/// Create a note via --json and return its id.
fn create(data_dir: &Path, args: &[&str]) -> String {
    let out = kiem(data_dir).args(["create", "--json"]).args(args).output().unwrap();
    assert!(out.status.success(), "create failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn create_list_show_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let id = create(dir.path(), &["--body", "# Test\n\nHello world #demo"]);

    kiem(dir.path())
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("Test").and(predicate::str::contains(&id)));

    kiem(dir.path())
        .args(["show", &id])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Hello world")
                .and(predicate::str::contains("tags:     demo")),
        );
}

#[test]
fn title_flag_becomes_h1_derived_title() {
    let dir = tempfile::tempdir().unwrap();
    let id = create(dir.path(), &["--title", "Groceries", "--body", "milk"]);
    let out = kiem(dir.path()).args(["show", &id, "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["title"], "Groceries");
    assert_eq!(v["body"], "# Groceries\n\nmilk");
}

#[test]
fn create_from_stdin() {
    let dir = tempfile::tempdir().unwrap();
    kiem(dir.path())
        .args(["create", "--json"])
        .write_stdin("# Piped\n\nfrom stdin")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"title\": \"Piped\""));
}

#[test]
fn create_with_nothing_fails() {
    let dir = tempfile::tempdir().unwrap();
    kiem(dir.path())
        .arg("create")
        .assert()
        .failure()
        .stderr(predicate::str::contains("provide --body"));
}

#[test]
fn search_finds_created_note() {
    let dir = tempfile::tempdir().unwrap();
    create(dir.path(), &["--body", "# Animals\n\nthe okapi hides"]);
    create(dir.path(), &["--body", "# Other\n\nnothing here"]);
    kiem(dir.path())
        .args(["search", "okapi"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Animals").and(predicate::str::contains("okapi")));
}

#[test]
fn search_json_is_a_valid_array() {
    let dir = tempfile::tempdir().unwrap();
    create(dir.path(), &["--body", "# A\n\nzebra"]);
    let out = kiem(dir.path()).args(["search", "zebra", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert!(v[0]["score"].as_f64().unwrap() > 0.0);
}

#[test]
fn tags_lists_unique_tags_with_counts() {
    let dir = tempfile::tempdir().unwrap();
    create(dir.path(), &["--body", "# A\n\nx #shared #solo"]);
    create(dir.path(), &["--body", "# B\n\ny #shared"]);
    kiem(dir.path())
        .arg("tags")
        .assert()
        .success()
        .stdout(predicate::str::contains("shared (2)").and(predicate::str::contains("solo (1)")));
}

#[test]
fn edit_replaces_body() {
    let dir = tempfile::tempdir().unwrap();
    let id = create(dir.path(), &["--body", "# Before"]);
    kiem(dir.path())
        .args(["edit", &id, "--body", "# After\n\nnew"])
        .assert()
        .success()
        .stdout(predicate::str::contains("After"));
    kiem(dir.path())
        .args(["show", &id])
        .assert()
        .stdout(predicate::str::contains("# After"));
}

#[test]
fn delete_moves_note_out_of_list() {
    let dir = tempfile::tempdir().unwrap();
    let id = create(dir.path(), &["--body", "# Goner"]);
    kiem(dir.path()).args(["delete", &id]).assert().success();
    kiem(dir.path())
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("Goner").not());
}

#[test]
fn list_json_is_a_valid_array_and_empty_store_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let out = kiem(dir.path()).args(["list", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);

    kiem(dir.path()).arg("list").assert().success().stdout("");

    create(dir.path(), &["--body", "# One"]);
    let out = kiem(dir.path()).args(["list", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["title"], "One");
}

#[test]
fn list_filters_by_tag() {
    let dir = tempfile::tempdir().unwrap();
    create(dir.path(), &["--body", "# W\n\nabout #work"]);
    create(dir.path(), &["--body", "# H\n\nabout #home"]);
    kiem(dir.path())
        .args(["list", "--tag", "work"])
        .assert()
        .success()
        .stdout(predicate::str::contains("W").and(predicate::str::contains("H  ").not()));
}

#[test]
fn show_and_edit_unknown_id_fail_with_message() {
    let dir = tempfile::tempdir().unwrap();
    kiem(dir.path())
        .args(["show", "no-such-id"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("note not found: no-such-id"));
    kiem(dir.path())
        .args(["edit", "no-such-id", "--body", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("note not found: no-such-id"));
}

// -- projects & agent loop (U4, U5) --

/// A kiem command run with both a data dir and a working directory (for project
/// resolution from the `.kiem` marker / directory name).
fn kiem_in(data_dir: &Path, work_dir: &Path) -> Command {
    let mut cmd = kiem(data_dir);
    cmd.current_dir(work_dir);
    cmd
}

fn json_out(cmd: &mut Command) -> serde_json::Value {
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "command failed: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn project_current_resolves_marker_then_dirname() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    // No marker → slugified directory name.
    let from_name = json_out(kiem_in(data.path(), repo.path()).args(["project", "current", "--json"]));
    assert!(from_name["project"].as_str().unwrap().starts_with("proj/"));

    // `project add` writes the marker; `current` then resolves to it.
    kiem_in(data.path(), repo.path()).args(["project", "add", "Demo Project"]).assert().success();
    let from_marker = json_out(kiem_in(data.path(), repo.path()).args(["project", "current", "--json"]));
    assert_eq!(from_marker["project"], "proj/demo_project");
    assert!(repo.path().join(".kiem").is_file(), "marker committed to repo");
    assert!(repo.path().join("AGENTS.md").is_file(), "AGENTS.md pointer written");
}

#[test]
fn project_add_is_idempotent_no_duplicate_home_note() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let first = json_out(kiem_in(data.path(), repo.path()).args(["project", "add", "Demo", "--json"]));
    assert!(first["home_note"].is_string(), "first add creates a home note");

    let second = json_out(kiem_in(data.path(), repo.path()).args(["project", "add", "Demo", "--json"]));
    assert!(second["home_note"].is_null(), "re-add binds without a second home note");

    // Exactly one project, one note.
    let projects = json_out(kiem_in(data.path(), repo.path()).args(["project", "list", "--json"]));
    assert_eq!(projects.as_array().unwrap().len(), 1);
}

#[test]
fn agent_loop_add_note_list_todos_then_check() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    kiem_in(data.path(), repo.path()).args(["project", "add", "Demo"]).assert().success();

    // Add a note with two open todos.
    let note = json_out(
        kiem_in(data.path(), repo.path())
            .args(["note", "add", "# Tasks\n- [ ] first\n- [ ] second", "--json"]),
    );
    let note_id = note["id"].as_str().unwrap().to_owned();

    // `todos` aggregates the project's open items (home note has none).
    let todos = json_out(kiem_in(data.path(), repo.path()).args(["todos", "--json"]));
    let items = todos.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["note_id"], note_id.as_str());
    assert_eq!(items[0]["index"], 0);

    // Check the first todo → it drops out of the aggregate.
    kiem(data.path()).args(["todo", "check", &note_id, "0"]).assert().success();
    let after = json_out(kiem_in(data.path(), repo.path()).args(["todos", "--json"]));
    assert_eq!(after.as_array().unwrap().len(), 1);
    assert_eq!(after[0]["text"], "second");
}

#[test]
fn note_add_does_not_duplicate_an_already_present_project_tag() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    kiem_in(data.path(), repo.path()).args(["project", "add", "Demo"]).assert().success();

    // Text already carries the project tag — it must not be appended again.
    let note = json_out(
        kiem_in(data.path(), repo.path())
            .args(["note", "add", "# Hand tagged\n\n#proj/demo", "--json"]),
    );
    let id = note["id"].as_str().unwrap().to_owned();
    let shown = json_out(kiem(data.path()).args(["show", &id, "--json"]));
    let body = shown["body"].as_str().unwrap();
    assert_eq!(body.matches("#proj/demo").count(), 1, "body: {body:?}");
    assert_eq!(shown["tags"].as_array().unwrap(), &[serde_json::json!("proj/demo")]);
}

#[test]
fn todo_add_appends_one_item_in_a_single_command() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    kiem_in(data.path(), repo.path()).args(["project", "add", "Demo"]).assert().success();
    let note = json_out(
        kiem_in(data.path(), repo.path()).args(["note", "add", "# Tasks\n- [ ] first", "--json"]),
    );
    let id = note["id"].as_str().unwrap().to_owned();

    // One command, no whole-body rewrite — the new item is addressable as index 1.
    kiem(data.path()).args(["todo", "add", &id, "second"]).assert().success();
    let todos = json_out(kiem_in(data.path(), repo.path()).args(["todos", "--json"]));
    let items = todos.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[1]["text"], "second");

    // Empty text is rejected rather than writing a blank checkbox.
    kiem(data.path()).args(["todo", "add", &id, "   "]).assert().failure();
}

#[test]
fn todo_check_bad_index_fails() {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    kiem_in(data.path(), repo.path()).args(["project", "add", "Demo"]).assert().success();
    let note = json_out(
        kiem_in(data.path(), repo.path()).args(["note", "add", "# T\n- [ ] only", "--json"]),
    );
    let note_id = note["id"].as_str().unwrap().to_owned();
    kiem(data.path())
        .args(["todo", "check", &note_id, "9"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("out of range"));
}
