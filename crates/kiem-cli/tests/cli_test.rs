//! End-to-end CLI tests against a temp data directory: the note surface.
//! Projects and todos are in `cli_project_test.rs`, bulk in
//! `cli_bulk_test.rs`.

use predicates::prelude::*;

mod common;
use common::{create, json_out, kiem, kiem_in};

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
            predicate::str::contains("Hello world").and(predicate::str::contains("tags:     demo")),
        );
}

#[test]
fn title_flag_becomes_h1_derived_title() {
    let dir = tempfile::tempdir().unwrap();
    let id = create(dir.path(), &["--title", "Groceries", "--body", "milk"]);
    let out = kiem(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
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
    let out = kiem(dir.path())
        .args(["search", "zebra", "--json"])
        .output()
        .unwrap();
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

#[test]
fn note_ref_is_accepted_in_every_id_position() {
    let dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    kiem_in(dir.path(), repo.path())
        .args(["project", "add", "Demo"])
        .assert()
        .success();
    let note = json_out(kiem_in(dir.path(), repo.path()).args([
        "note",
        "add",
        "# Tasks\n- [ ] first\n- [ ] second",
        "--json",
    ]));
    let id = note["id"].as_str().unwrap().to_owned();
    let ref_ = format!("kiem://note/{id}");
    let ref_with_slash = format!("kiem://note/{id}/");

    // All forms point to the same note.
    let from_bare = json_out(kiem(dir.path()).args(["show", &id, "--json"]));
    let from_ref = json_out(kiem(dir.path()).args(["show", &ref_, "--json"]));
    let from_ref_slash = json_out(kiem(dir.path()).args(["show", &ref_with_slash, "--json"]));
    assert_eq!(from_bare["id"], id);
    assert_eq!(from_ref["id"], id);
    assert_eq!(from_ref_slash["id"], id);

    // Todo commands also accept the reference.
    kiem(dir.path())
        .args(["todo", "check", &ref_, "0"])
        .assert()
        .success();
    let todos = json_out(kiem_in(dir.path(), repo.path()).args(["todos", "--json"]));
    assert_eq!(todos.as_array().unwrap().len(), 1);

    // Edit and delete via ref work too. Keep the project tag so the edit is allowed.
    kiem(dir.path())
        .args(["edit", &ref_, "--body", "# Updated\n\n#proj/demo"])
        .assert()
        .success();
    kiem(dir.path()).args(["delete", &ref_]).assert().success();
}

// -- projects & agent loop (U4, U5) --
