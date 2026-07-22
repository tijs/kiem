//! Exercises `session::run` over a real (loopback) iroh connection, not just
//! the framing logic. Bounded by a timeout: this needs to bind real UDP
//! sockets, which a fully network-isolated sandbox may refuse — a timeout
//! failure there means "no local networking available", not a protocol bug.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiem_core::note::NoteDoc;
use kiem_core::store::NoteStore;
use kiem_core::sync::SyncEngine;
use kiem_sync::SharedState;

const TS: &str = "2026-01-01T00:00:00Z";

fn empty_state() -> SharedState {
    Arc::new(Mutex::new((
        NoteStore::open_in_memory_with_search().unwrap(),
        SyncEngine::new(),
    )))
}

/// Aborts a spawned task when dropped, so a live iroh session (and its UDP
/// socket) doesn't leak if a later assertion panics before the test's own
/// explicit cleanup would otherwise run.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test]
async fn two_peers_converge_a_note_over_a_real_iroh_connection() {
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let a_ep = kiem_sync::bind(iroh::SecretKey::generate()).await.unwrap();
        let b_ep = kiem_sync::bind(iroh::SecretKey::generate()).await.unwrap();
        let b_addr = b_ep.addr();

        let a_state = empty_state();
        let b_state = empty_state();
        a_state
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

        // Each side records the peer it saw during the pairing prelude, so the
        // test can assert one connection makes trust reciprocal (unit 1).
        let (a_id, b_id) = (a_ep.id(), b_ep.id());
        let a_seen: Arc<Mutex<Vec<kiem_sync::EndpointId>>> = Arc::new(Mutex::new(Vec::new()));
        let b_seen: Arc<Mutex<Vec<kiem_sync::EndpointId>>> = Arc::new(Mutex::new(Vec::new()));
        let a_handshake = kiem_sync::PeerHandshake {
            local_ticket: kiem_sync::my_ticket(&a_ep).to_string(),
            local_name: "A".into(),
            on_peer: {
                let seen = a_seen.clone();
                Arc::new(move |addr| seen.lock().unwrap().push(addr.id))
            },
            on_name: Arc::new(|_peer, _name| {}),
            on_sync_activity: Arc::new(|_peer| {}),
        };
        let b_handshake = kiem_sync::PeerHandshake {
            local_ticket: kiem_sync::my_ticket(&b_ep).to_string(),
            local_name: "B".into(),
            on_peer: {
                let seen = b_seen.clone();
                Arc::new(move |addr| seen.lock().unwrap().push(addr.id))
            },
            on_name: Arc::new(|_peer, _name| {}),
            on_sync_activity: Arc::new(|_peer| {}),
        };

        let accept_task = tokio::spawn({
            let b_ep = b_ep.clone();
            let b_state = b_state.clone();
            async move {
                let conn = kiem_sync::accept(&b_ep).await.unwrap().unwrap();
                kiem_sync::run_session(conn, false, b_state, Duration::from_millis(20), b_handshake)
                    .await
            }
        });

        let conn = kiem_sync::connect(&a_ep, b_addr).await.unwrap();
        let connect_task = tokio::spawn(kiem_sync::run_session(
            conn,
            true,
            a_state.clone(),
            Duration::from_millis(20),
            a_handshake,
        ));

        let mut synced = false;
        for _ in 0..200 {
            if b_state.lock().unwrap().0.get_note("n1").unwrap().is_some() {
                synced = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        accept_task.abort();
        connect_task.abort();
        assert!(
            synced,
            "note did not sync from A to B over the iroh connection"
        );
        // One connection paired both directions: each side recorded the other.
        assert!(
            a_seen.lock().unwrap().contains(&b_id),
            "A did not record B from the pairing prelude"
        );
        assert!(
            b_seen.lock().unwrap().contains(&a_id),
            "B did not record A from the pairing prelude"
        );
    })
    .await;

    assert!(
        outcome.is_ok(),
        "timed out waiting for a loopback iroh connection — likely no local networking in this environment"
    );
}

/// Reproduces finding cd6c4fab: the outbound ticker used to call
/// `on_sync_activity` on every round even when it had nothing to send,
/// keeping a fully-converged peer stuck showing "Syncing" forever. Once a
/// real sync has settled, further idle ticker rounds must not keep bumping
/// the activity count (before the fix, they did — once per tick, forever).
#[tokio::test]
async fn idle_ticker_rounds_do_not_count_as_sync_activity_after_convergence() {
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let a_ep = kiem_sync::bind(iroh::SecretKey::generate()).await.unwrap();
        let b_ep = kiem_sync::bind(iroh::SecretKey::generate()).await.unwrap();
        let b_addr = b_ep.addr();

        let a_state = empty_state();
        let b_state = empty_state();
        a_state
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

        let a_activity: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
        let a_handshake = kiem_sync::PeerHandshake {
            local_ticket: kiem_sync::my_ticket(&a_ep).to_string(),
            local_name: "A".into(),
            on_peer: Arc::new(|_addr| {}),
            on_name: Arc::new(|_peer, _name| {}),
            on_sync_activity: {
                let count = a_activity.clone();
                Arc::new(move |_peer| *count.lock().unwrap() += 1)
            },
        };
        let b_handshake = kiem_sync::PeerHandshake {
            local_ticket: kiem_sync::my_ticket(&b_ep).to_string(),
            local_name: "B".into(),
            on_peer: Arc::new(|_addr| {}),
            on_name: Arc::new(|_peer, _name| {}),
            on_sync_activity: Arc::new(|_peer| {}),
        };

        let _accept_task = AbortOnDrop(tokio::spawn({
            let b_ep = b_ep.clone();
            let b_state = b_state.clone();
            async move {
                let conn = kiem_sync::accept(&b_ep).await.unwrap().unwrap();
                kiem_sync::run_session(conn, false, b_state, Duration::from_millis(20), b_handshake)
                    .await
            }
        }));
        let conn = kiem_sync::connect(&a_ep, b_addr).await.unwrap();
        let _connect_task = AbortOnDrop(tokio::spawn(kiem_sync::run_session(
            conn,
            true,
            a_state.clone(),
            Duration::from_millis(20),
            a_handshake,
        )));

        let mut synced = false;
        for _ in 0..200 {
            if b_state.lock().unwrap().0.get_note("n1").unwrap().is_some() {
                synced = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(synced, "note did not sync over the loopback connection");

        // Poll until the activity count stops growing (converged) instead of
        // hoping a fixed sleep was long enough — this baseline legitimately
        // includes the handshake + note exchange.
        let mut settled_count = *a_activity.lock().unwrap();
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let current = *a_activity.lock().unwrap();
            if current == settled_count {
                break;
            }
            settled_count = current;
        }
        assert!(settled_count > 0, "a real sync produced no activity calls");

        // Back to idle: further ticker rounds after convergence must not
        // keep bumping the count (this is the exact bug — it used to, once
        // per 20ms tick).
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            *a_activity.lock().unwrap(),
            settled_count,
            "activity kept firing on idle rounds after convergence instead of settling"
        );
    })
    .await;

    assert!(
        outcome.is_ok(),
        "timed out waiting for a loopback iroh connection — likely no local networking in this environment"
    );
}

/// The pairing ticket must carry a relay hint: without one the receiving
/// side dials by bare EndpointId and pays 20–35s of cold discovery on first
/// connect (finding df5ddfeb). `pair_ticket` waits for relay registration.
#[tokio::test]
async fn pair_ticket_carries_a_relay_hint() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        let ticket = kiem_sync::pair_ticket(dir.path()).await.unwrap();
        let addr = kiem_sync::parse_ticket(&ticket).unwrap();
        assert!(
            addr.relay_urls().next().is_some(),
            "ticket has no relay hint — pair_ticket read the address before relay registration"
        );
    })
    .await;
    assert!(
        outcome.is_ok(),
        "timed out reaching a relay — likely no networking in this environment"
    );
}
