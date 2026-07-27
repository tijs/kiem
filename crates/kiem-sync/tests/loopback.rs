//! Exercises `session::run` over a real (loopback) iroh connection, not just
//! the framing logic. Bounded by a timeout: this needs to bind real UDP
//! sockets, which a fully network-isolated sandbox may refuse — a timeout
//! failure there means "no local networking available", not a protocol bug.
//! For that message to stay true the endpoints must not reach for anything
//! beyond loopback; see `bind_loopback` and `warm_network_stack`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiem_core::note::NoteDoc;
use kiem_sync::SyncState;

mod common;
use common::{bind_loopback, empty_state, handshake, AbortOnDrop};

const TS: &str = "2026-01-01T00:00:00Z";

#[tokio::test]
async fn two_peers_converge_a_note_over_a_real_iroh_connection() {
    // Bound before the clock starts: see `bind_loopback` and `warm_network_stack` — the first bind in the
    // process initialises iroh's global network stack, which is neither
    // instant nor anything this test is asserting about.
    let a_ep = bind_loopback().await;
    let b_ep = bind_loopback().await;
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let b_addr = b_ep.addr();

        let a_state = empty_state();
        let b_state = empty_state();
        a_state
            .lock()
            .store
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
            if b_state.lock().store.get_note("n1").unwrap().is_some() {
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
    // Bound before the clock starts: see `bind_loopback` and `warm_network_stack` — the first bind in the
    // process initialises iroh's global network stack, which is neither
    // instant nor anything this test is asserting about.
    let a_ep = bind_loopback().await;
    let b_ep = bind_loopback().await;
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let b_addr = b_ep.addr();

        let a_state = empty_state();
        let b_state = empty_state();
        a_state
            .lock()
            .store
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
            if b_state.lock().store.get_note("n1").unwrap().is_some() {
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

/// The production disconnect path, which no other test covers: a live
/// `session::run` ends, its last line calls `SyncEngine::reset_peer`, and the
/// peers reconnect. Two claims, both about that call:
///
/// 1. What it leaves behind is *resumable* — the opening message of the next
///    connection summarises only what changed since the last agreed heads. A
///    fresh engine has to Bloom-summarise the document's whole change graph,
///    which is what the old `retain`-based `forget_peer` forced on every
///    reconnect (the bug this fixes).
/// 2. That retained state still converges a *new* connection. Stale sync state
///    meeting a fresh session is exactly the shape of this project's past
///    livelocks, so eventual replication is asserted, not assumed.
///
/// The fiddly part is ending the session for real: aborting the task (what the
/// tests above do) skips the `reset_peer` line entirely. Closing the connection
/// makes both reader loops return, so `run` finishes normally on both sides.
#[tokio::test]
async fn a_session_that_ends_leaves_resumable_state_and_the_next_one_replicates() {
    // Bound before the clock starts: see `bind_loopback` and `warm_network_stack`.
    let a_ep = bind_loopback().await;
    let b_ep = bind_loopback().await;
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        let b_addr = b_ep.addr();
        let b_peer = b_ep.id().to_string();
        let (a_state, b_state) = (empty_state(), empty_state());

        // Enough history that "summarise the whole document" costs visibly
        // more than "summarise what changed since we last agreed" — same
        // fixture shape as the unit test in kiem-core's sync.rs.
        a_state
            .lock()
            .store
            .insert_note(&NoteDoc::new_with(
                "n1".into(),
                "# History",
                "did:a",
                TS.into(),
            ))
            .unwrap();
        for i in 0..200 {
            a_state
                .lock()
                .store
                .update_note("n1", &format!("# History\n\nedit {i}"))
                .unwrap();
        }

        let noise = Arc::new(AtomicUsize::new(0));
        let a_hs = handshake(kiem_sync::my_ticket(&a_ep).to_string(), "A", noise.clone());
        let b_hs = handshake(kiem_sync::my_ticket(&b_ep).to_string(), "B", noise.clone());
        let tick = Duration::from_millis(20);

        // Session 1: converge.
        let accept_task = tokio::spawn({
            let (b_ep, b_state, b_hs) = (b_ep.clone(), b_state.clone(), b_hs.clone());
            async move {
                let conn = kiem_sync::accept(&b_ep).await.unwrap().unwrap();
                kiem_sync::run_session(conn, false, b_state, tick, b_hs).await
            }
        });
        let conn = kiem_sync::connect(&a_ep, b_addr.clone()).await.unwrap();
        let closer = conn.clone();
        let connect_task = tokio::spawn(kiem_sync::run_session(
            conn,
            true,
            a_state.clone(),
            tick,
            a_hs.clone(),
        ));

        let mut synced = false;
        for _ in 0..200 {
            if b_state.lock().store.get_note("n1").unwrap().is_some() {
                synced = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(synced, "note did not sync over the first session");

        // B holding the note is not yet agreement: A's shared heads only
        // advance when B's reply lands. Disconnecting on the arrival of the
        // note alone leaves A with nothing to resume *from*, and the claim
        // below then fails for a reason that has nothing to do with
        // `reset_peer`. Wait for the exchange to go quiet instead — the same
        // settle-then-assert shape the idle-ticker test above uses.
        let mut settled = noise.load(Ordering::SeqCst);
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let current = noise.load(Ordering::SeqCst);
            if current == settled {
                break;
            }
            settled = current;
        }

        // Disconnect for real: both `run` calls return, so both run their
        // `reset_peer`. Awaiting the handles is what makes that ordering
        // guaranteed rather than racy.
        closer.close(0u32.into(), b"test done");
        let _ = connect_task.await;
        let _ = accept_task.await;

        // Claim 1: the state left behind resumes from the shared heads.
        let resumed = {
            let SyncState { store, engine } = &mut *a_state.lock();
            engine
                .generate_message(store, &b_peer, "n1")
                .unwrap()
                .expect("a reconnect still opens with a sync message")
                .len()
        };
        let cold = {
            let store = &mut a_state.lock().store;
            kiem_core::sync::SyncEngine::new()
                .generate_message(store, &b_peer, "n1")
                .unwrap()
                .expect("a cold engine opens with a sync message")
                .len()
        };
        assert!(
            resumed * 2 < cold,
            "the session's reset_peer did not retain shared heads: reconnect \
             opens with {resumed} bytes vs {cold} for a cold engine"
        );

        // Claim 2: a new note still replicates over the reconnect.
        a_state
            .lock()
            .store
            .insert_note(&NoteDoc::new_with(
                "n2".into(),
                "# After the reconnect",
                "did:a",
                TS.into(),
            ))
            .unwrap();

        let _accept_task = AbortOnDrop(tokio::spawn({
            let (b_ep, b_state) = (b_ep.clone(), b_state.clone());
            async move {
                let conn = kiem_sync::accept(&b_ep).await.unwrap().unwrap();
                kiem_sync::run_session(conn, false, b_state, tick, b_hs).await
            }
        }));
        let conn = kiem_sync::connect(&a_ep, b_addr).await.unwrap();
        let _connect_task = AbortOnDrop(tokio::spawn(kiem_sync::run_session(
            conn,
            true,
            a_state.clone(),
            tick,
            a_hs,
        )));

        let mut resynced = false;
        for _ in 0..200 {
            if b_state.lock().store.get_note("n2").unwrap().is_some() {
                resynced = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            resynced,
            "the note written after the disconnect never replicated over the reconnect"
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
    // `pair_ticket` binds its own endpoint, so it would otherwise pay the
    // one-time network-stack init inside the budget (see `bind_loopback` and `warm_network_stack`).
    drop(bind_loopback().await);
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

