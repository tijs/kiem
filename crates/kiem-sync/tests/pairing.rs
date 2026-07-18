//! End-to-end pairing over a real (loopback) iroh mesh: the trust gate, the
//! approval hook, and the forced pairing dial working together. Like
//! `loopback.rs`, this binds real UDP sockets — a timeout here means "no local
//! networking in this sandbox", not a protocol bug.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiem_core::note::NoteDoc;
use kiem_core::store::NoteStore;
use kiem_core::sync::SyncEngine;
use kiem_sync::{EndpointId, KnownPeers, Mesh, MeshEvents, NoEvents, SharedState, PEERS_FILE};

const TS: &str = "2026-01-01T00:00:00Z";
const INTERVAL: Duration = Duration::from_millis(50);

/// Approves every incoming pairing — stands in for a user tapping "Allow".
struct ApproveAll;
impl MeshEvents for ApproveAll {
    fn approve_pairing(&self, _peer: EndpointId) -> bool {
        true
    }
}

fn empty_state() -> SharedState {
    Arc::new(Mutex::new((
        NoteStore::open_in_memory_with_search().unwrap(),
        SyncEngine::new(),
    )))
}

fn state_with_note() -> SharedState {
    let state = empty_state();
    state
        .lock()
        .unwrap()
        .0
        .insert_note(&NoteDoc::new_with(
            "n1".into(),
            "# Hello\n\nfrom A",
            "did:a",
            TS.into(),
        ))
        .unwrap();
    state
}

fn knows(data_dir: &std::path::Path, id: &EndpointId) -> bool {
    KnownPeers::load(&data_dir.join(PEERS_FILE))
        .map(|k| k.contains(id))
        .unwrap_or(false)
}

/// An armed + approved window lets an unknown peer pair in one forced dial:
/// the note syncs and both sides record each other's trust.
#[tokio::test]
async fn approved_pairing_syncs_and_records_mutual_trust() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        let state_a = state_with_note();
        let state_b = empty_state();

        let mesh_b = Mesh::start(
            dir_b.path().into(),
            state_b.clone(),
            INTERVAL,
            Arc::new(ApproveAll),
        )
        .await
        .unwrap();
        let mesh_a = Mesh::start(
            dir_a.path().into(),
            state_a.clone(),
            INTERVAL,
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
        let (a_id, b_id) = (mesh_a.endpoint_id(), mesh_b.endpoint_id());

        mesh_b.arm_pairing(Duration::from_secs(60));
        let b_addr = kiem_sync::parse_ticket(&mesh_b.ticket()).unwrap();
        mesh_a.pair_dial(b_addr);

        let mut synced = false;
        for _ in 0..300 {
            if state_b.lock().unwrap().0.get_note("n1").unwrap().is_some() {
                synced = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(synced, "note did not sync after an approved pairing");
        assert!(
            knows(dir_b.path(), &a_id),
            "B did not record A as a trusted peer"
        );
        assert!(
            knows(dir_a.path(), &b_id),
            "A did not record B as a trusted peer"
        );
        assert!(
            mesh_b.pairing_window_remaining().is_none(),
            "an approved pairing must consume the single-use window"
        );
    })
    .await;
    assert!(
        outcome.is_ok(),
        "timed out — likely no local networking in this environment"
    );
}

/// A denied pairing (default-deny approval) trusts nobody and — crucially —
/// leaves the window open, so the real device can still pair.
#[tokio::test]
async fn a_denied_pairing_leaves_the_window_open() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        let state_a = state_with_note();
        let state_b = empty_state();

        // B arms a window but denies every prompt (NoEvents = default-deny).
        let mesh_b = Mesh::start(
            dir_b.path().into(),
            state_b.clone(),
            INTERVAL,
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
        let mesh_a = Mesh::start(
            dir_a.path().into(),
            state_a.clone(),
            INTERVAL,
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
        let a_id = mesh_a.endpoint_id();

        mesh_b.arm_pairing(Duration::from_secs(60));
        let b_addr = kiem_sync::parse_ticket(&mesh_b.ticket()).unwrap();
        mesh_a.pair_dial(b_addr);

        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !knows(dir_b.path(), &a_id),
            "a denied peer must not be trusted"
        );
        assert!(
            mesh_b.pairing_window_remaining().is_some(),
            "a denied pairing must leave the window open for the real device"
        );
    })
    .await;
    assert!(
        outcome.is_ok(),
        "timed out — likely no local networking in this environment"
    );
}

/// A ticket from a running mesh must carry a relay hint (via `ticket_online`),
/// or the peer pays cold discovery on first connect (the df5ddfeb finding).
#[tokio::test]
async fn running_mesh_ticket_carries_a_relay_hint() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        let mesh = Mesh::start(
            dir.path().into(),
            empty_state(),
            INTERVAL,
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
        let addr = kiem_sync::parse_ticket(&mesh.ticket_online().await).unwrap();
        assert!(
            addr.relay_urls().next().is_some(),
            "running-mesh ticket has no relay hint — ticket_online didn't wait for registration"
        );
    })
    .await;
    assert!(
        outcome.is_ok(),
        "timed out reaching a relay — likely no networking in this environment"
    );
}

/// With no window open, an unknown peer dialing in is refused before any sync:
/// nothing is recorded and no note crosses. (The prelude records a peer within
/// the first round-trip, so a few seconds with no record means genuine refusal,
/// not lag.)
#[tokio::test]
async fn unknown_peer_is_refused_when_no_window_is_open() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        let state_a = state_with_note();
        let state_b = empty_state();

        // B: default-deny events and no armed window.
        let mesh_b = Mesh::start(
            dir_b.path().into(),
            state_b.clone(),
            INTERVAL,
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
        let mesh_a = Mesh::start(
            dir_a.path().into(),
            state_a.clone(),
            INTERVAL,
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
        let a_id = mesh_a.endpoint_id();

        let b_addr = kiem_sync::parse_ticket(&mesh_b.ticket()).unwrap();
        mesh_a.pair_dial(b_addr);

        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !knows(dir_b.path(), &a_id),
            "B trusted an unknown peer with no pairing window"
        );
        assert!(
            state_b.lock().unwrap().0.get_note("n1").unwrap().is_none(),
            "a note synced to B despite the pairing being refused"
        );
    })
    .await;
    assert!(
        outcome.is_ok(),
        "timed out — likely no local networking in this environment"
    );
}
