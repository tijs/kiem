//! `SyncEngine` behaviour, driven entirely through its public API — two
//! in-memory peers pumping messages at each other with no transport involved.
//!
//! Lives here rather than as a `#[cfg(test)] mod tests` inside `sync.rs`:
//! these are integration tests of the engine + store pair, and keeping them
//! out is what holds `sync.rs` itself to a readable size.

use kiem_core::note::NoteDoc;
use kiem_core::store::NoteStore;
use kiem_core::sync::SyncEngine;

const TS: &str = "2026-06-12T10:00:00Z";

struct Peer {
    name: &'static str,
    store: NoteStore,
    engine: SyncEngine,
}

fn peer(name: &'static str) -> Peer {
    Peer {
        name,
        store: NoteStore::open_in_memory_with_search().unwrap(),
        engine: SyncEngine::new(),
    }
}

/// Pump every pending message from one peer to the other; returns count.
fn pump(from: &mut Peer, to: &mut Peer) -> usize {
    let mut sent = 0;
    for id in from.engine.doc_ids(&from.store).unwrap() {
        if let Some(msg) = from
            .engine
            .generate_message(&from.store, to.name, &id)
            .unwrap()
        {
            to.engine
                .receive_message(&mut to.store, from.name, &id, &msg)
                .unwrap();
            sent += 1;
        }
    }
    sent
}

/// Exchange messages in both directions until neither side has anything
/// to say. Returns the number of messages that crossed the wire. Flushes
/// both sides' deferred search-index writes on convergence, mirroring
/// what the real transport's per-tick `flush_search_index` call does in
/// production (see session.rs's `sync_round`).
fn converge(a: &mut Peer, b: &mut Peer) -> usize {
    let mut total = 0;
    loop {
        let round = pump(a, b) + pump(b, a);
        if round == 0 {
            a.store.flush_search_index().unwrap();
            b.store.flush_search_index().unwrap();
            return total;
        }
        total += round;
    }
}

#[test]
fn note_created_on_one_peer_appears_on_the_other() {
    let (mut a, mut b) = (peer("a"), peer("b"));
    let note = NoteDoc::new_with("n1".into(), "# Hello\n\nfrom A #sync", "did:a", TS.into());
    a.store.insert_note(&note).unwrap();

    converge(&mut a, &mut b);

    let got = b.store.get_note("n1").unwrap().expect("synced to B");
    assert_eq!(got.body.as_str(), "# Hello\n\nfrom A #sync");
    assert_eq!(got.metadata.title, "Hello");
    // B's denormalized columns and search index follow the synced doc.
    assert_eq!(b.store.list_by_tag("sync").unwrap().len(), 1);
    assert_eq!(b.store.search("Hello", 10).unwrap().len(), 1);
}

#[test]
fn peers_with_independent_notes_end_up_with_all_of_them() {
    let (mut a, mut b) = (peer("a"), peer("b"));
    a.store
        .insert_note(&NoteDoc::new_with(
            "na".into(),
            "# From A",
            "did:a",
            TS.into(),
        ))
        .unwrap();
    b.store
        .insert_note(&NoteDoc::new_with(
            "nb".into(),
            "# From B",
            "did:b",
            TS.into(),
        ))
        .unwrap();

    converge(&mut a, &mut b);

    for p in [&a, &b] {
        let ids: Vec<String> = p
            .store
            .list_notes()
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids.len(), 2, "{} should have both notes", p.name);
    }
}

#[test]
fn concurrent_edits_to_the_same_note_merge_on_both_sides() {
    let (mut a, mut b) = (peer("a"), peer("b"));
    let note = NoteDoc::new_with("n1".into(), "# Shared\n\nbase", "did:a", TS.into());
    a.store.insert_note(&note).unwrap();
    converge(&mut a, &mut b);

    // Diverge: A appends one line, B appends a different one.
    a.store
        .update_note("n1", "# Shared\n\nbase\nline from A")
        .unwrap();
    b.store
        .update_note("n1", "# Shared\n\nbase\nline from B")
        .unwrap();
    converge(&mut a, &mut b);

    let body_a = a
        .store
        .get_note("n1")
        .unwrap()
        .unwrap()
        .body
        .as_str()
        .to_owned();
    let body_b = b
        .store
        .get_note("n1")
        .unwrap()
        .unwrap()
        .body
        .as_str()
        .to_owned();
    assert_eq!(body_a, body_b, "peers must converge to identical bodies");
    assert!(body_a.contains("line from A") && body_a.contains("line from B"));
}

#[test]
fn converged_peers_exchange_minimal_traffic() {
    let (mut a, mut b) = (peer("a"), peer("b"));
    a.store
        .insert_note(&NoteDoc::new_with("n1".into(), "# X", "did:a", TS.into()))
        .unwrap();
    converge(&mut a, &mut b);
    // Re-running produces no document traffic beyond (at most) one
    // already-in-sync handshake message per direction per doc.
    let second = converge(&mut a, &mut b);
    assert!(second <= 2, "expected near-zero traffic, got {second}");
}

/// One idle "tick": generate (and drop) messages for every doc, as the
/// transport's `sync_round` does.
fn tick(p: &mut Peer, to: &str) {
    for id in p.engine.doc_ids(&p.store).unwrap() {
        p.engine.generate_message(&p.store, to, &id).unwrap();
    }
}

#[test]
fn unchanged_docs_skip_blob_fetches_after_first_tick() {
    let (mut a, mut b) = (peer("a"), peer("b"));
    for i in 0..10 {
        a.store
            .insert_note(&NoteDoc::new_with(
                format!("n{i}"),
                &format!("# Note {i}"),
                "did:a",
                TS.into(),
            ))
            .unwrap();
    }
    converge(&mut a, &mut b);

    // First idle tick may rebuild caches invalidated by the last
    // receive; every tick after that must be BLOB-free.
    tick(&mut a, "b");
    let after_first = a.engine.note_blob_fetches();
    for _ in 0..5 {
        tick(&mut a, "b");
    }
    assert_eq!(
        a.engine.note_blob_fetches(), after_first,
        "idle ticks over unchanged docs must not fetch note BLOBs"
    );
}

#[test]
fn edit_between_ticks_is_picked_up_on_the_next_tick() {
    let (mut a, mut b) = (peer("a"), peer("b"));
    a.store
        .insert_note(&NoteDoc::new_with("n1".into(), "# X", "did:a", TS.into()))
        .unwrap();
    converge(&mut a, &mut b);
    // Prime the watermark so the skip path is active, then write behind
    // the engine's back the way the CLI/app does.
    tick(&mut a, "b");
    tick(&mut a, "b");
    a.store.update_note("n1", "# X\n\nedited between ticks").unwrap();

    converge(&mut a, &mut b);

    assert!(b
        .store
        .get_note("n1")
        .unwrap()
        .unwrap()
        .body
        .as_str()
        .contains("edited between ticks"));
}

#[test]
fn soft_delete_propagates() {
    let (mut a, mut b) = (peer("a"), peer("b"));
    a.store
        .insert_note(&NoteDoc::new_with("n1".into(), "# Bye", "did:a", TS.into()))
        .unwrap();
    converge(&mut a, &mut b);
    a.store.delete_note("n1").unwrap();
    converge(&mut a, &mut b);

    assert!(b.store.list_notes().unwrap().is_empty());
    assert_eq!(b.store.list_deleted().unwrap().len(), 1);
    assert!(
        b.store.search("Bye", 10).unwrap().is_empty(),
        "trashed note left B's index"
    );
}

#[test]
fn interrupted_sync_resumes_with_fresh_state() {
    // A peer restart drops sync states; the next handshake must still
    // converge (states are an optimization, not a correctness carrier).
    let (mut a, mut b) = (peer("a"), peer("b"));
    a.store
        .insert_note(&NoteDoc::new_with(
            "n1".into(),
            "# Resume",
            "did:a",
            TS.into(),
        ))
        .unwrap();
    converge(&mut a, &mut b);

    b.engine = SyncEngine::new(); // B "restarted"
    a.engine.reset_peer("b");
    a.store
        .update_note("n1", "# Resume\n\nafter restart")
        .unwrap();
    converge(&mut a, &mut b);

    assert!(b
        .store
        .get_note("n1")
        .unwrap()
        .unwrap()
        .body
        .as_str()
        .contains("after restart"));
}

/// Two peers converged on one note with a long change history — enough
/// changes that "summarise the whole document" costs visibly more on the
/// wire than "summarise what changed since we last agreed".
///
/// The change count is the headroom knob for the guard below, and it is
/// also what this helper costs (it runs twice, ~13s in a debug build at
/// 300) — see the numbers on that assertion before raising it further.
fn converged_pair_with_history() -> (Peer, Peer) {
    let (mut a, mut b) = (peer("a"), peer("b"));
    a.store
        .insert_note(&NoteDoc::new_with(
            "n1".into(),
            "# History",
            "did:a",
            TS.into(),
        ))
        .unwrap();
    for i in 0..300 {
        a.store
            .update_note("n1", &format!("# History\n\nedit {i}"))
            .unwrap();
    }
    converge(&mut a, &mut b);
    (a, b)
}

/// Like `converge`, but returns the bytes that crossed the wire.
fn converge_bytes(a: &mut Peer, b: &mut Peer) -> usize {
    fn pump_bytes(from: &mut Peer, to: &mut Peer) -> usize {
        let mut bytes = 0;
        for id in from.engine.doc_ids(&from.store).unwrap() {
            if let Some(msg) = from
                .engine
                .generate_message(&from.store, to.name, &id)
                .unwrap()
            {
                bytes += msg.len();
                to.engine
                    .receive_message(&mut to.store, from.name, &id, &msg)
                    .unwrap();
            }
        }
        bytes
    }
    let mut total = 0;
    loop {
        let round = pump_bytes(a, b) + pump_bytes(b, a);
        if round == 0 {
            return total;
        }
        total += round;
    }
}

#[test]
fn reconnecting_resumes_from_shared_heads_instead_of_re_summarising() {
    // A disconnect (reset_peer) must stay cheaper than a process restart
    // (states gone): the reconnect handshake should Bloom-summarise only
    // what changed since the last agreed heads, not the whole document.
    let (mut a, mut b) = converged_pair_with_history();
    a.engine.reset_peer("b");
    b.engine.reset_peer("a");
    let resumed = converge_bytes(&mut a, &mut b);

    let (mut a, mut b) = converged_pair_with_history();
    a.engine = SyncEngine::new();
    b.engine = SyncEngine::new();
    let cold = converge_bytes(&mut a, &mut b);

    // Measured on this fixture (one note, 300 changes): 183 bytes resumed
    // vs 533 cold — the assertion clears by ~1.5x. `resumed` is flat in
    // the change count and `cold` grows with it (~1.2 bytes/change), so
    // the fixture size is the headroom knob: 200 changes left only ~10%,
    // which is close enough to the edge that a change in automerge's
    // Bloom sizing would flip this red with nothing here broken.
    assert!(
        resumed * 2 < cold,
        "reconnect after a disconnect should resume from shared heads: \
         {resumed} bytes vs {cold} for a cold restart"
    );
}

#[test]
fn forgetting_a_peer_drops_its_state_so_the_next_contact_is_cold() {
    // The mirror image of the test above: unpairing must *not* keep the
    // shared heads. Same fixture, so the two numbers are comparable — a
    // forgotten peer costs exactly what a never-seen one costs.
    let (mut a, mut b) = converged_pair_with_history();
    a.engine.forget_peer("b");
    b.engine.forget_peer("a");
    let forgotten = converge_bytes(&mut a, &mut b);

    let (mut a, mut b) = converged_pair_with_history();
    a.engine = SyncEngine::new();
    b.engine = SyncEngine::new();
    let cold = converge_bytes(&mut a, &mut b);

    assert_eq!(
        forgotten, cold,
        "forget_peer left state behind: a forgotten peer re-handshaked in \
         {forgotten} bytes where a never-seen one costs {cold}"
    );
}

/// Like `converge`, but bails out after `max_rounds` instead of looping
/// forever — a livelock should fail the test, not hang the suite.
fn converge_bounded(a: &mut Peer, b: &mut Peer, max_rounds: usize) -> Option<usize> {
    let mut total = 0;
    for _ in 0..max_rounds {
        let round = pump(a, b) + pump(b, a);
        if round == 0 {
            a.store.flush_search_index().unwrap();
            b.store.flush_search_index().unwrap();
            return Some(total);
        }
        total += round;
    }
    None
}

/// Reproduces a livelock reported live (finding baf2d005): two peers with
/// ~600 already-converged notes both restart near-simultaneously — unlike
/// `interrupted_sync_resumes_with_fresh_state`, which only resets *one*
/// side, here *both* engines reset at once, so both sides need a fresh
/// per-doc handshake simultaneously. Live symptom: continuous non-empty
/// sync activity forever with zero notes ever actually crossing.
#[test]
fn both_peers_restarting_simultaneously_still_converges() {
    let (mut a, mut b) = (peer("a"), peer("b"));
    for i in 0..600 {
        a.store
            .insert_note(&NoteDoc::new_with(
                format!("shared-{i}"),
                &format!("# Note {i}"),
                "did:a",
                TS.into(),
            ))
            .unwrap();
    }
    converge_bounded(&mut a, &mut b, 10_000).expect("initial convergence of 600 notes");
    assert_eq!(b.store.list_notes().unwrap().len(), 600);

    // Both peers "restart" at once: both engines reset, unlike the
    // single-sided reset in interrupted_sync_resumes_with_fresh_state.
    a.engine = SyncEngine::new();
    b.engine = SyncEngine::new();

    a.store
        .insert_note(&NoteDoc::new_with(
            "new-from-a".into(),
            "# New from A",
            "did:a",
            TS.into(),
        ))
        .unwrap();
    b.store
        .insert_note(&NoteDoc::new_with(
            "new-from-b".into(),
            "# New from B",
            "did:b",
            TS.into(),
        ))
        .unwrap();

    let result = converge_bounded(&mut a, &mut b, 10_000);
    assert!(
        result.is_some(),
        "did not converge within 10,000 rounds after a simultaneous double restart"
    );

    assert!(
        a.store.get_note("new-from-b").unwrap().is_some(),
        "A never got B's new note"
    );
    assert!(
        b.store.get_note("new-from-a").unwrap().is_some(),
        "B never got A's new note"
    );
}
