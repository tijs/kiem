//! Two real `kiem sync` daemon processes converging over a real iroh
//! connection, paired via `kiem pair` tickets instead of LAN discovery.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// Generous: dial-by-bare-EndpointId depends on iroh's discovery/relay
// resolution, which can take tens of seconds depending on network
// conditions — see the finding in kiem-sync's mesh tests.
const WAIT: Duration = Duration::from_secs(45);
const POLL: Duration = Duration::from_millis(250);

/// Kills the daemon on drop so no test leaves orphan processes.
struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn kiem_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kiem"))
}

fn kiem(data_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(kiem_bin())
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .output()
        .expect("kiem command runs")
}

fn kiem_json(data_dir: &Path, args: &[&str]) -> serde_json::Value {
    let out = kiem(data_dir, args);
    assert!(
        out.status.success(),
        "kiem {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("valid JSON output")
}

/// Pairs two devices the way a user would: A arms `pair show` and waits, B
/// runs `pair add` with A's code. One action per side; the reciprocal
/// handshake records the trust on both — no second add.
fn pair(dir_a: &Path, dir_b: &Path) {
    let mut show = Command::new(kiem_bin())
        .arg("--data-dir")
        .arg(dir_a)
        .args(["pair", "show", "--yes", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("pair show spawns");

    // The first stdout line is the ticket (printed as soon as it's known;
    // `show` then keeps waiting for the pairing).
    let mut ticket_line = String::new();
    BufReader::new(show.stdout.as_mut().unwrap())
        .read_line(&mut ticket_line)
        .expect("pair show prints a ticket");
    let ticket_json: serde_json::Value = serde_json::from_str(&ticket_line).expect("ticket JSON");
    let ticket = ticket_json["ticket"].as_str().unwrap();

    let added = kiem_json(dir_b, &["pair", "add", "--json", ticket]);
    assert_eq!(
        added["paired"].as_str(),
        Some(ticket_a_id(dir_a).as_str()),
        "pair add must report the show side as paired: {added}"
    );

    let show_out = show.wait_with_output().expect("pair show exits");
    assert!(show_out.status.success(), "pair show failed after the add");
}

/// The show side's endpoint id, read from its data dir's persisted identity.
fn ticket_a_id(dir_a: &Path) -> String {
    kiem_sync::device_id(dir_a).unwrap().to_string()
}

fn spawn_daemon(data_dir: &Path) -> DaemonGuard {
    let child = Command::new(kiem_bin())
        .arg("--data-dir")
        .arg(data_dir)
        .args(["sync", "--interval-ms", "200"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("daemon spawns");
    DaemonGuard(child)
}

/// Poll `predicate` against `kiem list --json` until it holds.
fn wait_for_notes(data_dir: &Path, predicate: impl Fn(&serde_json::Value) -> bool) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        let notes = kiem_json(data_dir, &["list", "--json"]);
        if predicate(&notes) {
            return;
        }
        std::thread::sleep(POLL);
    }
    panic!(
        "condition not reached; final state: {}",
        kiem_json(data_dir, &["list", "--json"])
    );
}

#[test]
fn two_daemons_converge_and_stay_in_sync() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    // A note created on A before B even exists.
    kiem_json(dir_a.path(), &["create", "--json", "--body", "# From A\n\nearly note #sync"]);

    pair(dir_a.path(), dir_b.path());
    let _daemon_a = spawn_daemon(dir_a.path());
    let _daemon_b = spawn_daemon(dir_b.path());

    // AE5/AE2: B receives the pre-existing note after connecting.
    wait_for_notes(dir_b.path(), |notes| {
        notes.as_array().is_some_and(|a| {
            a.iter().any(|n| n["title"] == "From A" && n["tags"][0] == "sync")
        })
    });

    // Live edit while both daemons run: create on B, expect it on A.
    kiem_json(dir_b.path(), &["create", "--json", "--body", "# From B\n\nlive note"]);
    wait_for_notes(dir_a.path(), |notes| {
        notes.as_array().is_some_and(|a| a.iter().any(|n| n["title"] == "From B"))
    });

    // Edit A's note on B; the merged body must flow back to A.
    let notes_b = kiem_json(dir_b.path(), &["list", "--json"]);
    let id = notes_b
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["title"] == "From A")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    kiem_json(dir_b.path(), &["edit", &id, "--json", "--body", "# From A\n\nedited on B #sync"]);
    wait_for_notes(dir_a.path(), |_| {
        let shown = kiem_json(dir_a.path(), &["show", &id, "--json"]);
        shown["body"].as_str().is_some_and(|b| b.contains("edited on B"))
    });

    // Both daemons report each other as connected. The status file is
    // heartbeat-published (1s cadence), so poll rather than assert once.
    for dir in [dir_a.path(), dir_b.path()] {
        let deadline = Instant::now() + WAIT;
        loop {
            let status = kiem_json(dir, &["sync-status", "--json"]);
            if status["peers"].as_array().is_some_and(|p| p.len() == 1) {
                break;
            }
            assert!(Instant::now() < deadline, "peer never showed up in {status}");
            std::thread::sleep(POLL);
        }
    }
}
