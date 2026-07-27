//! Shared harness for the CLI test binaries: a `kiem` command bound to a
//! temp data dir, and the two shapes every test needs from it.

#![allow(dead_code)] // each test binary uses a different subset

use std::path::Path;

use assert_cmd::Command;

pub fn kiem(data_dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("kiem").unwrap();
    cmd.arg("--data-dir").arg(data_dir);
    cmd
}

/// Create a note via --json and return its id.
pub fn create(data_dir: &Path, args: &[&str]) -> String {
    let out = kiem(data_dir)
        .args(["create", "--json"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// resolution from the `.kiem` marker / directory name).
pub fn kiem_in(data_dir: &Path, work_dir: &Path) -> Command {
    let mut cmd = kiem(data_dir);
    cmd.current_dir(work_dir);
    cmd
}

pub fn json_out(cmd: &mut Command) -> serde_json::Value {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
