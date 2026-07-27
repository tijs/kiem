//! End-to-end pairing over a real (loopback) iroh mesh: the trust gate, the
//! approval hook, and the forced pairing dial working together. Like
//! `loopback.rs`, this binds real UDP sockets — a timeout here means "no local
//! networking in this sandbox", not a protocol bug. Unlike `loopback.rs` these
//! go through `Mesh`, so they use the product's real relay/discovery preset;
//! only the one-time network-stack init is kept off the clock.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kiem_core::note::NoteDoc;
use kiem_core::store::NoteStore;
use kiem_sync::{EndpointId, KnownPeers, Mesh, MeshEvents, NoEvents, SharedState, PEERS_FILE};

mod common;

const TS: &str = "2026-01-01T00:00:00Z";
const INTERVAL: Duration = Duration::from_millis(50);

/// Approves every incoming pairing — stands in for a user tapping "Allow".
struct ApproveAll;
impl MeshEvents for ApproveAll {
    fn approve_pairing(&self, _peer: EndpointId) -> bool {
        true
    }
}

/// Counts the connections a mesh actually established.
///
/// The two refusal tests assert *negatives* — nobody trusted, window still
/// open, no note crossed — which a dial that never lands satisfies just as
/// well as a dial that lands and is refused. That is precisely how they used
/// to pass on this machine. Asserting this counter is what makes them say
/// "refused a real connection" instead of "nothing happened".
///
/// The dialing side is the one that counts: `admit_incoming` lets our own
/// dialed connections through (we only dial peers we already trust), so A
/// reports connected as soon as the QUIC handshake completes, whether or not
/// B then refuses us.
#[derive(Clone, Default)]
struct CountConnections(Arc<AtomicUsize>);

impl CountConnections {
    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl MeshEvents for CountConnections {
    fn on_connected(&self, _peer: EndpointId) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn empty_state() -> SharedState {
    kiem_sync::shared_state(NoteStore::open_in_memory_with_search().unwrap())
}

fn state_with_note() -> SharedState {
    let state = empty_state();
    state
        .lock()
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
    // Bound before the clock starts, and loopback-only: a ticket read from a
    // mesh bound the product way carries this machine's LAN and Tailscale
    // addresses, and dialing those hairpins back to the same host times out —
    // so these tests used to assert about a connection that never formed.
    let (ep_a, ep_b) = (common::bind_loopback().await, common::bind_loopback().await);
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        let state_a = state_with_note();
        let state_b = empty_state();

        let mesh_b = Mesh::start_with_endpoint(
            ep_b,
            dir_b.path().into(),
            state_b.clone(),
            INTERVAL,
            Arc::new(ApproveAll),
        )
        .await
        .unwrap();
        let mesh_a = Mesh::start_with_endpoint(
            ep_a,
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
            if state_b.lock().0.get_note("n1").unwrap().is_some() {
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

/// Unpairing is pairing's inverse and has to hold on a *live* mesh: forgetting
/// a device must stop the sync that is happening right now, not just edit a
/// file that takes effect on the next launch.
///
/// The negative (B's later note never arrives) is only meaningful because the
/// test first proves the two were genuinely syncing — the vacuity trap this
/// suite has been bitten by before.
#[tokio::test]
async fn forgetting_a_device_stops_syncing_with_it() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    // A is deliberately the smaller EndpointId, so A is the side running the
    // guarded dial loop (see `dial_loop`) — the loop unpairing has to stop.
    // Left to chance, half the runs would put that loop on B and never
    // exercise it. B being refused at A's gate is the other half of the story
    // and is already covered by the denied-pairing test.
    let (ep_a, ep_b) = {
        let (x, y) = (common::bind_loopback().await, common::bind_loopback().await);
        if x.id() < y.id() {
            (x, y)
        } else {
            (y, x)
        }
    };
    let outcome = tokio::time::timeout(Duration::from_secs(60), async {
        let state_a = state_with_note();
        let state_b = empty_state();

        let mesh_b = Mesh::start_with_endpoint(
            ep_b,
            dir_b.path().into(),
            state_b.clone(),
            INTERVAL,
            Arc::new(ApproveAll),
        )
        .await
        .unwrap();
        let mesh_a = Mesh::start_with_endpoint(
            ep_a,
            dir_a.path().into(),
            state_a.clone(),
            INTERVAL,
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
        let b_id = mesh_b.endpoint_id();

        mesh_b.arm_pairing(Duration::from_secs(60));
        mesh_a.pair_dial(kiem_sync::parse_ticket(&mesh_b.ticket()).unwrap());

        // Precondition: they really are paired and syncing.
        let mut synced = false;
        for _ in 0..300 {
            if state_b.lock().0.get_note("n1").unwrap().is_some() {
                synced = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(synced, "the two never paired, so forgetting proves nothing");
        assert!(knows(dir_a.path(), &b_id), "A did not record B");

        assert!(
            mesh_a.forget_peer(&b_id).unwrap(),
            "forget_peer should report B as a known peer"
        );
        assert!(
            !knows(dir_a.path(), &b_id),
            "B is still in A's trust list after being forgotten"
        );

        // B writes a note it would have synced a second ago.
        state_b
            .lock()
            .0
            .insert_note(&NoteDoc::new_with(
                "n2".into(),
                "# After unpairing",
                "did:b",
                TS.into(),
            ))
            .unwrap();

        // Well past RECONNECT_DELAY: long enough that a dial loop that ignored
        // the unpair, or a gate that admitted B back, would have resynced.
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(
                state_a.lock().0.get_note("n2").unwrap().is_none(),
                "a forgotten device is still syncing to us"
            );
        }
        assert!(
            !mesh_a.connected_ids().contains(&b_id),
            "the session with the forgotten device is still open"
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
    // Bound before the clock starts, and loopback-only: a ticket read from a
    // mesh bound the product way carries this machine's LAN and Tailscale
    // addresses, and dialing those hairpins back to the same host times out —
    // so these tests used to assert about a connection that never formed.
    let (ep_a, ep_b) = (common::bind_loopback().await, common::bind_loopback().await);
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        let state_a = state_with_note();
        let state_b = empty_state();

        // B arms a window but denies every prompt (NoEvents = default-deny).
        let mesh_b = Mesh::start_with_endpoint(
            ep_b,
            dir_b.path().into(),
            state_b.clone(),
            INTERVAL,
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
        let a_connections = CountConnections::default();
        let mesh_a = Mesh::start_with_endpoint(
            ep_a,
            dir_a.path().into(),
            state_a.clone(),
            INTERVAL,
            Arc::new(a_connections.clone()),
        )
        .await
        .unwrap();
        let a_id = mesh_a.endpoint_id();

        mesh_b.arm_pairing(Duration::from_secs(60));
        let b_addr = kiem_sync::parse_ticket(&mesh_b.ticket()).unwrap();
        mesh_a.pair_dial(b_addr);

        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            a_connections.count() > 0,
            "the pairing dial never landed — the refusal below would pass vacuously"
        );
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
    // Before the clock starts, not inside it: `Mesh::start` binds an endpoint,
    // and the first bind in the process pays a one-time global init that has
    // nothing to do with pairing (see `common::warm_network_stack`).
    common::warm_network_stack().await;
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
    // Bound before the clock starts, and loopback-only: a ticket read from a
    // mesh bound the product way carries this machine's LAN and Tailscale
    // addresses, and dialing those hairpins back to the same host times out —
    // so these tests used to assert about a connection that never formed.
    let (ep_a, ep_b) = (common::bind_loopback().await, common::bind_loopback().await);
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        let state_a = state_with_note();
        let state_b = empty_state();

        // B: default-deny events and no armed window.
        let mesh_b = Mesh::start_with_endpoint(
            ep_b,
            dir_b.path().into(),
            state_b.clone(),
            INTERVAL,
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
        let a_connections = CountConnections::default();
        let mesh_a = Mesh::start_with_endpoint(
            ep_a,
            dir_a.path().into(),
            state_a.clone(),
            INTERVAL,
            Arc::new(a_connections.clone()),
        )
        .await
        .unwrap();
        let a_id = mesh_a.endpoint_id();

        let b_addr = kiem_sync::parse_ticket(&mesh_b.ticket()).unwrap();
        mesh_a.pair_dial(b_addr);

        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            a_connections.count() > 0,
            "the dial never landed — the refusals below would pass vacuously"
        );
        assert!(
            !knows(dir_b.path(), &a_id),
            "B trusted an unknown peer with no pairing window"
        );
        assert!(
            state_b.lock().0.get_note("n1").unwrap().is_none(),
            "a note synced to B despite the pairing being refused"
        );
    })
    .await;
    assert!(
        outcome.is_ok(),
        "timed out — likely no local networking in this environment"
    );
}
