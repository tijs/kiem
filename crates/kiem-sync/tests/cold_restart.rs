//! Reproduction for the "simultaneous cold restart livelocks sync on a large
//! store" finding: both peers hold the same few hundred already-converged
//! documents on disk, both have empty in-memory `SyncEngine` state (that map
//! does not survive a restart), and each has one note the other has never
//! seen. The two new notes must still replicate.
//!
//! Bounded by a timeout: this binds real UDP sockets, so a timeout in a
//! network-isolated sandbox means "no local networking", not a protocol bug.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kiem_core::note::NoteDoc;
use kiem_core::sync::SyncEngine;
use kiem_sync::SharedState;

mod common;
use common::{bind_loopback, empty_state, handshake, AbortOnDrop};

const TS: &str = "2026-01-01T00:00:00Z";
/// Sized for a debug-profile `cargo test` run, not for the reported 610-note
/// store: automerge is roughly two orders of magnitude slower unoptimised, so
/// a faithful count here would dominate the suite. Raise it (and use
/// `--release`) when probing scale rather than correctness.
const SHARED_DOCS: usize = 80;

/// Bring both stores to the state a converged pair of peers is in *before*
/// the restart: A creates `SHARED_DOCS` notes, then the two are driven to
/// convergence in-process, off the wire. The engines used for that are then
/// thrown away — which is precisely what a restart does to `SyncEngine`,
/// whose state lives in memory only.
fn seed_converged_then_forget_engines(a: &SharedState, b: &SharedState) {
    for i in 0..SHARED_DOCS {
        a.lock()
            .store
            .insert_note(&NoteDoc::new_with(
                format!("shared-{i:04}"),
                &format!("# Shared {i}\n\nbody {i}"),
                "did:a",
                TS.into(),
            ))
            .unwrap();
    }

    let (mut ea, mut eb) = (SyncEngine::new(), SyncEngine::new());
    // Automerge's sync protocol needs a few round trips per document; loop
    // until both sides go quiet.
    for _ in 0..10 {
        let mut quiet = true;
        let ids = {
            let store = &a.lock().store;
            ea.doc_ids(store).unwrap()
        };
        for id in &ids {
            let msg = {
                let store = &a.lock().store;
                ea.generate_message(store, "b", id).unwrap()
            };
            if let Some(msg) = msg {
                quiet = false;
                let store = &mut b.lock().store;
                eb.receive_message(store, "a", id, &msg).unwrap();
            }
            let reply = {
                let store = &b.lock().store;
                eb.generate_message(store, "a", id).unwrap()
            };
            if let Some(reply) = reply {
                quiet = false;
                let store = &mut a.lock().store;
                ea.receive_message(store, "b", id, &reply).unwrap();
            }
        }
        if quiet {
            break;
        }
    }
    b.lock().store.flush_search_index().unwrap();
    assert_eq!(
        b.lock().store.list_all_ids().unwrap().len(),
        SHARED_DOCS,
        "seeding did not converge the two stores"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cold_restart_with_a_large_shared_store_still_replicates_new_notes() {
    // Bound before the clock starts: see `bind_loopback` and `warm_network_stack`.
    let a_ep = bind_loopback().await;
    let b_ep = bind_loopback().await;
    let outcome = tokio::time::timeout(Duration::from_secs(120), async {
        let b_addr = b_ep.addr();

        let a_state = empty_state();
        let b_state = empty_state();
        seed_converged_then_forget_engines(&a_state, &b_state);

        // The post-restart edit on each side: one note the peer has never seen.
        a_state
            .lock()
            .store
            .insert_note(&NoteDoc::new_with(
                "only-on-a".into(),
                "# From A\n\nafter restart",
                "did:a",
                TS.into(),
            ))
            .unwrap();
        b_state
            .lock()
            .store
            .insert_note(&NoteDoc::new_with(
                "only-on-b".into(),
                "# From B\n\nafter restart",
                "did:b",
                TS.into(),
            ))
            .unwrap();

        let a_activity = Arc::new(AtomicUsize::new(0));
        let b_activity = Arc::new(AtomicUsize::new(0));
        let a_handshake = handshake(
            kiem_sync::my_ticket(&a_ep).to_string(),
            "A",
            a_activity.clone(),
        );
        let b_handshake = handshake(
            kiem_sync::my_ticket(&b_ep).to_string(),
            "B",
            b_activity.clone(),
        );

        // The real app's tick, not the 20ms the small loopback tests use —
        // the reported symptom is about what a 1s tick does to a big store.
        let interval = Duration::from_millis(1000);
        let _accept_task = AbortOnDrop(tokio::spawn({
            let b_ep = b_ep.clone();
            let b_state = b_state.clone();
            async move {
                let conn = kiem_sync::accept(&b_ep).await.unwrap().unwrap();
                kiem_sync::run_session(conn, false, b_state, interval, b_handshake).await
            }
        }));
        let conn = kiem_sync::connect(&a_ep, b_addr).await.unwrap();
        let _connect_task = AbortOnDrop(tokio::spawn(kiem_sync::run_session(
            conn,
            true,
            a_state.clone(),
            interval,
            a_handshake,
        )));

        let start = std::time::Instant::now();
        let mut converged = false;
        for _ in 0..400 {
            let a_has_b = a_state
                .lock()
                .store
                .get_note("only-on-b")
                .unwrap()
                .is_some();
            let b_has_a = b_state
                .lock()
                .store
                .get_note("only-on-a")
                .unwrap()
                .is_some();
            if a_has_b && b_has_a {
                converged = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let elapsed = start.elapsed();
        eprintln!(
            "converged={converged} after {elapsed:?}; activity a={} b={}",
            a_activity.load(Ordering::Relaxed),
            b_activity.load(Ordering::Relaxed)
        );
        assert!(
            converged,
            "the two post-restart notes never replicated across a {SHARED_DOCS}-doc \
             cold-start store (elapsed {elapsed:?}, sync activity a={} b={})",
            a_activity.load(Ordering::Relaxed),
            b_activity.load(Ordering::Relaxed)
        );
    })
    .await;

    assert!(
        outcome.is_ok(),
        "timed out — either no local networking, or sync made no progress at all"
    );
}

/// The other half of the reported scenario: both sides have a *large* amount
/// to push at the same time. That is what a real double restart looks like
/// when the two stores have genuinely diverged, and it is the case the small
/// loopback tests never cover.
///
/// It matters because of how the session multiplexes one QUIC stream: the
/// ticker takes the `send` mutex and holds it for its whole batch, while the
/// reader must take that same mutex to answer each frame it reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_peers_pushing_a_large_backlog_at_once_still_converge() {
    // Bound before the clock starts: see `bind_loopback` and `warm_network_stack`.
    let a_ep = bind_loopback().await;
    let b_ep = bind_loopback().await;
    let outcome = tokio::time::timeout(Duration::from_secs(90), async {
        let b_addr = b_ep.addr();

        let a_state = empty_state();
        let b_state = empty_state();

        // Big enough, in both directions at once, to exhaust the QUIC stream
        // flow-control window rather than fitting in one buffer.
        const EACH: usize = 15;
        let filler = "lorem ipsum dolor sit amet ".repeat(1500); // ~40KB
        for (state, side) in [(&a_state, "a"), (&b_state, "b")] {
            for i in 0..EACH {
                state
                    .lock()
                    .store
                    .insert_note(&NoteDoc::new_with(
                        format!("{side}-{i:04}"),
                        &format!("# {side} {i}\n\n{filler}"),
                        "did:x",
                        TS.into(),
                    ))
                    .unwrap();
            }
        }

        let noise = Arc::new(AtomicUsize::new(0));
        let a_handshake = handshake(kiem_sync::my_ticket(&a_ep).to_string(), "A", noise.clone());
        let b_handshake = handshake(kiem_sync::my_ticket(&b_ep).to_string(), "B", noise.clone());

        let interval = Duration::from_millis(1000);
        let _accept_task = AbortOnDrop(tokio::spawn({
            let b_ep = b_ep.clone();
            let b_state = b_state.clone();
            async move {
                let conn = kiem_sync::accept(&b_ep).await.unwrap().unwrap();
                kiem_sync::run_session(conn, false, b_state, interval, b_handshake).await
            }
        }));
        let conn = kiem_sync::connect(&a_ep, b_addr).await.unwrap();
        let _connect_task = AbortOnDrop(tokio::spawn(kiem_sync::run_session(
            conn,
            true,
            a_state.clone(),
            interval,
            a_handshake,
        )));

        let start = std::time::Instant::now();
        let mut last = (0, 0);
        let mut converged = false;
        for _ in 0..300 {
            let a_count = a_state.lock().store.list_all_ids().unwrap().len();
            let b_count = b_state.lock().store.list_all_ids().unwrap().len();
            if (a_count, b_count) != last {
                eprintln!(
                    "{:?} a={a_count} b={b_count} activity={}",
                    start.elapsed(),
                    noise.load(Ordering::Relaxed)
                );
                last = (a_count, b_count);
            }
            if a_count == EACH * 2 && b_count == EACH * 2 {
                converged = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(
            converged,
            "a simultaneous two-way backlog never converged: a={} b={} of {} after {:?}",
            last.0,
            last.1,
            EACH * 2,
            start.elapsed()
        );
    })
    .await;

    assert!(
        outcome.is_ok(),
        "timed out — either no local networking, or the session made no progress"
    );
}
