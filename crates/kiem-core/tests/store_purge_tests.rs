//! Permanent erasure and how it travels: Empty Trash, deleting a project,
//! and the tombstone document that stops a peer resurrecting a purged note.

use kiem_core::store::NoteStore;

mod common;
use common::{note, store_with};

#[test]
fn purges_propagate_to_peers_and_win_against_offline_edits() {
    use kiem_core::sync::SyncEngine;

    // Two stores that have fully converged on one note.
    let mut store_a = store_with(&[note("n1", "# Doomed note", "2026-06-28T10:00:00Z")]);
    let mut store_b = NoteStore::open_in_memory().unwrap();
    let mut engine_a = SyncEngine::new();
    let mut engine_b = SyncEngine::new();
    sync_until_converged(&mut store_a, &mut engine_a, &mut store_b, &mut engine_b);
    assert_eq!(store_b.list_notes().unwrap().len(), 1);

    // B trashes and permanently erases it; concurrently ("offline") A edits it.
    store_b.delete_note("n1").unwrap();
    assert_eq!(store_b.purge_deleted().unwrap(), 1);
    assert!(store_b.get_note("n1").unwrap().is_none());
    store_a
        .update_note("n1", "# Doomed note\n\nedited while B purged")
        .unwrap();

    // Full exchange: the purge wins everywhere. B must not resurrect the note
    // from A's edit (put_doc drops purged ids), and A must adopt the purge
    // through the synced tombstone doc.
    sync_until_converged(&mut store_a, &mut engine_a, &mut store_b, &mut engine_b);
    assert!(
        store_b.get_note("n1").unwrap().is_none(),
        "purged note resurrected on B"
    );
    assert!(
        store_a.get_note("n1").unwrap().is_none(),
        "purge did not propagate to A"
    );
    assert!(store_a.list_deleted().unwrap().is_empty());
    assert!(store_b.list_deleted().unwrap().is_empty());

    // A note created after the purge syncs normally in both directions.
    store_a
        .create_note("# Survivor", "did:key:z6MkTest")
        .unwrap();
    sync_until_converged(&mut store_a, &mut engine_a, &mut store_b, &mut engine_b);
    assert_eq!(store_b.list_notes().unwrap().len(), 1);
}

/// Ping-pong sync messages for every doc id until both directions go quiet.
fn sync_until_converged(
    store_a: &mut NoteStore,
    engine_a: &mut kiem_core::sync::SyncEngine,
    store_b: &mut NoteStore,
    engine_b: &mut kiem_core::sync::SyncEngine,
) {
    for _ in 0..20 {
        let mut traffic = false;
        let mut ids = engine_a.doc_ids(store_a).unwrap();
        for id in engine_b.doc_ids(store_b).unwrap() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        for id in &ids {
            if let Some(msg) = engine_a.generate_message(store_a, "b", id).unwrap() {
                engine_b.receive_message(store_b, "a", id, &msg).unwrap();
                traffic = true;
            }
            if let Some(msg) = engine_b.generate_message(store_b, "a", id).unwrap() {
                engine_a.receive_message(store_a, "b", id, &msg).unwrap();
                traffic = true;
            }
        }
        if !traffic {
            return;
        }
    }
    panic!("sync did not converge");
}

#[test]
fn purge_tag_erases_the_whole_project_including_trashed_and_spares_others() {
    let mut store = store_with(&[
        note("x1", "# X home #proj/x", "2026-06-28T10:00:00Z"),
        note("x2", "# X note #proj/x", "2026-06-28T11:00:00Z"),
        note("x3", "# X trashed #proj/x", "2026-06-28T12:00:00Z"),
        note("y1", "# Y note #proj/y", "2026-06-28T13:00:00Z"),
    ]);
    store.delete_note("x3").unwrap();

    assert_eq!(store.purge_tag("proj/x").unwrap(), 3);

    assert!(store.list_by_tag("proj/x").unwrap().is_empty());
    assert!(store.get_note("x1").unwrap().is_none());
    assert!(
        store.get_note("x3").unwrap().is_none(),
        "trashed project note purged too"
    );
    assert!(store.list_deleted().unwrap().is_empty());
    // The other project is untouched, and the project tag is gone entirely.
    assert_eq!(store.list_by_tag("proj/y").unwrap().len(), 1);
    assert!(store
        .list_tags()
        .unwrap()
        .iter()
        .all(|(tag, _)| tag != "proj/x"));
}

#[test]
fn tombstone_adoption_mid_deferred_burst_does_not_stall_on_the_writer_lock() {
    use automerge::AutoCommit;

    // Disk-backed store: the tantivy writer lock is only real on disk.
    let dir = tempfile::tempdir().unwrap();
    let mut store_b = NoteStore::open_dir(dir.path()).unwrap();

    // A source store supplies the documents: one live note for the "burst",
    // three purged ids carried by its tombstone doc.
    let mut store_a = store_with(&[note("burst-1", "# Burst", "2026-06-28T10:00:00Z")]);
    for id in ["p1", "p2", "p3"] {
        store_a
            .insert_note(&note(id, "# Doomed", "2026-06-28T10:00:00Z"))
            .unwrap();
        store_a.delete_note(id).unwrap();
    }
    assert_eq!(store_a.purge_deleted().unwrap(), 3);

    // Open B's deferred search-index writer, as the sync receive path does
    // during a document burst.
    let bytes = store_a.get_doc_bytes("burst-1").unwrap().unwrap();
    let mut doc = AutoCommit::load(&bytes).unwrap();
    store_b.put_doc_deferred(&mut doc).unwrap();

    // Adopting a tombstone doc used to do an *immediate* index removal per
    // purged id, each burning the full ~2s writer-lock retry budget against
    // the deferred writer opened above (finding baf2d005: 32 purged ids kept
    // the store mutex held for 64+ seconds, stalling sync). With deferred
    // removals the adoption reuses the open writer and completes instantly.
    let tomb = store_a.tombstone_doc_bytes().unwrap().unwrap();
    let mut tomb = AutoCommit::load(&tomb).unwrap();
    let started = std::time::Instant::now();
    store_b.adopt_tombstone_doc(&mut tomb).unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "tombstone adoption stalled on the search-index writer lock: {:?}",
        started.elapsed()
    );
}
