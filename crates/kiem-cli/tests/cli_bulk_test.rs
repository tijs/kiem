//! `kiem bulk`: the multi-note operations, their selector rules, and the
//! dry-run/--yes safety gate an agent has to pass to change anything.

use predicates::prelude::*;

mod common;
use common::{create, json_out, kiem};

#[test]
fn bulk_tag_remove_previews_then_allows_intentional_untagging() {
    let data = tempfile::tempdir().unwrap();
    let only = create(data.path(), &["--body", "# Only\n\n#proj/demo"]);
    let keep = create(data.path(), &["--body", "# Keep\n\n#proj/demo #keep"]);

    let preview = json_out(kiem(data.path()).args([
        "bulk",
        "--project",
        "demo",
        "--dry-run",
        "tag",
        "remove",
        "proj/demo",
        "--json",
    ]));
    assert_eq!(preview["would_change"], 2);
    assert_eq!(
        json_out(kiem(data.path()).args(["show", &only, "--json"]))["tags"],
        serde_json::json!(["proj/demo"])
    );

    kiem(data.path())
        .args([
            "bulk",
            "--project",
            "demo",
            "--yes",
            "tag",
            "remove",
            "proj/demo",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Changed 2 of 2"));
    assert_eq!(
        json_out(kiem(data.path()).args(["show", &only, "--json"]))["tags"],
        serde_json::json!([])
    );
    assert_eq!(
        json_out(kiem(data.path()).args(["show", &keep, "--json"]))["tags"],
        serde_json::json!(["keep"])
    );
}

#[test]
fn bulk_supports_tag_ids_stdin_type_delete_and_restore() {
    let data = tempfile::tempdir().unwrap();
    let a = create(data.path(), &["--body", "# A\n\n#source"]);
    let b = create(data.path(), &["--body", "# B\n\n#source"]);

    kiem(data.path())
        .args(["bulk", "--tag", "source", "--yes", "tag", "add", "target"])
        .assert()
        .success();
    kiem(data.path())
        .args(["bulk", "--stdin", "--yes", "set-type", "plan"])
        .write_stdin(format!("{a}\n{b}\n"))
        .assert()
        .success();
    assert_eq!(
        json_out(kiem(data.path()).args(["show", &a, "--json"]))["note_type"],
        "plan"
    );

    kiem(data.path())
        .args(["bulk", "--id", &a, "--id", &b, "--yes", "delete"])
        .assert()
        .success();
    kiem(data.path())
        .args(["bulk", "--tag", "target", "--yes", "restore"])
        .assert()
        .success();
    assert_eq!(
        json_out(kiem(data.path()).args(["list", "--tag", "target", "--json"]))
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn bulk_requires_one_selector_and_explicit_safety_flag() {
    let data = tempfile::tempdir().unwrap();
    create(data.path(), &["--body", "# A\n\n#source"]);
    kiem(data.path())
        .args(["bulk", "--tag", "source", "delete"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("requires --dry-run or --yes"));
    kiem(data.path())
        .args(["bulk", "--yes", "delete"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("choose exactly one selector"));
}
