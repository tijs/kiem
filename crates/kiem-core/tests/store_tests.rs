//! NoteStore behavior: CRUD, soft delete, smart filters, persistence.

use kiem_core::note::NoteDoc;
use kiem_core::store::{NoteStore, StoreError};

const DID: &str = "did:key:z6MkTest";

fn note(id: &str, body: &str, ts: &str) -> NoteDoc {
    NoteDoc::new_with(id.into(), body, DID, ts.into())
}

fn store_with(notes: &[NoteDoc]) -> NoteStore {
    let mut store = NoteStore::open_in_memory().unwrap();
    for n in notes {
        store.insert_note(n).unwrap();
    }
    store
}

#[test]
fn create_then_get_roundtrips_all_fields() {
    let mut store = NoteStore::open_in_memory().unwrap();
    let meta = store
        .create_note("# Hello\n\nWorld #greeting", DID)
        .unwrap();
    assert_eq!(meta.title, "Hello");
    assert_eq!(meta.tags, vec!["greeting"]);

    let loaded = store.get_note(&meta.id).unwrap().expect("note exists");
    assert_eq!(loaded.metadata, meta);
    assert_eq!(loaded.body.as_str(), "# Hello\n\nWorld #greeting");
}

#[test]
fn create_with_frontmatter_status_roundtrips_through_a_real_insert() {
    // Exercises the actual INSERT statement's column/param binding for
    // `status`, not just the migration test's hand-written SQL.
    let mut store = NoteStore::open_in_memory().unwrap();
    let body = "---\nstatus: active\n---\n# Plan\n\nbody #proj/x";
    let meta = store.create_note(body, DID).unwrap();
    assert_eq!(meta.status, Some("active".to_string()));
    assert_eq!(meta.title, "Plan");

    let loaded = store.get_note(&meta.id).unwrap().expect("note exists");
    assert_eq!(loaded.metadata.status, Some("active".to_string()));
    // The denormalized SQLite column agrees with the Automerge doc — list
    // queries read the column directly, never hydrate the doc.
    let listed = store.list_notes().unwrap();
    assert_eq!(listed[0].status, Some("active".to_string()));
}

#[test]
fn get_unknown_id_returns_none() {
    let store = store_with(&[]);
    assert!(store.get_note("nope").unwrap().is_none());
}

#[test]
fn duplicate_id_is_rejected() {
    let n = note("dup", "# A", "2026-06-12T10:00:00Z");
    let mut store = store_with(std::slice::from_ref(&n));
    match store.insert_note(&n) {
        Err(StoreError::DuplicateId(id)) => assert_eq!(id, "dup"),
        other => panic!("expected DuplicateId, got {other:?}"),
    }
}

#[test]
fn update_rederives_metadata_and_preserves_created_at() {
    let mut store = store_with(&[note("a", "# Before", "2026-06-10T10:00:00Z")]);
    let meta = store
        .update_note("a", "# After\n\n- [ ] task #later")
        .unwrap();
    assert_eq!(meta.title, "After");
    assert_eq!(meta.tags, vec!["later"]);
    assert_eq!(meta.created_at, "2026-06-10T10:00:00Z");
    assert!(
        meta.modified_at > meta.created_at,
        "modified_at must advance"
    );

    let loaded = store.get_note("a").unwrap().unwrap();
    assert_eq!(loaded.body.as_str(), "# After\n\n- [ ] task #later");
    assert_eq!(store.list_todos().unwrap().len(), 1);
}

#[test]
fn update_rederives_status_on_edit() {
    // Coverage gap: status was only ever exercised at creation time before
    // this test — verify it re-derives on an edit through the real update
    // path (write_body), in both directions (add, then remove).
    let mut store = store_with(&[note("a", "# Before", "2026-06-10T10:00:00Z")]);
    assert_eq!(store.get_note("a").unwrap().unwrap().metadata.status, None);

    let meta = store
        .update_note("a", "---\nstatus: active\n---\n# After #tag")
        .unwrap();
    assert_eq!(meta.status, Some("active".to_string()));

    let meta = store
        .update_note("a", "---\nstatus: completed\n---\n# After #tag")
        .unwrap();
    assert_eq!(meta.status, Some("completed".to_string()));

    // Removing the frontmatter clears status back to None.
    let meta = store.update_note("a", "# After #tag").unwrap();
    assert_eq!(meta.status, None);
    assert_eq!(store.get_note("a").unwrap().unwrap().metadata.status, None);
}

#[test]
fn update_unknown_id_is_not_found() {
    let mut store = store_with(&[]);
    assert!(matches!(
        store.update_note("ghost", "x"),
        Err(StoreError::NotFound(id)) if id == "ghost"
    ));
}

#[test]
fn update_preserves_crdt_history_in_stored_doc() {
    // The stored BLOB must accumulate changes (same document mutated), not be
    // a fresh document per write — sync depends on shared history.
    let mut store = store_with(&[note("a", "# V1", "2026-06-10T10:00:00Z")]);
    let mut before =
        automerge::AutoCommit::load(&store.get_doc_bytes("a").unwrap().unwrap()).unwrap();
    store.update_note("a", "# V1\n\nmore").unwrap();
    let mut after =
        automerge::AutoCommit::load(&store.get_doc_bytes("a").unwrap().unwrap()).unwrap();
    // The pre-update heads must be ancestors of the post-update doc.
    assert!(!before.get_heads().is_empty());
    let changes = after.get_changes(&before.get_heads());
    assert!(
        !changes.is_empty(),
        "updated doc must extend the original history"
    );
}

#[test]
fn soft_delete_retrievable_and_restorable() {
    let mut store = store_with(&[note("a", "# A", "2026-06-10T10:00:00Z")]);
    store.delete_note("a").unwrap();
    assert!(store.list_notes().unwrap().is_empty());
    assert_eq!(store.list_deleted().unwrap().len(), 1);
    assert!(store.get_note("a").unwrap().unwrap().metadata.deleted);

    store.restore_note("a").unwrap();
    assert_eq!(store.list_notes().unwrap().len(), 1);
    assert!(store.list_deleted().unwrap().is_empty());
}

#[test]
fn persists_across_open_close() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let mut store = NoteStore::open_dir(dir.path()).unwrap();
        store
            .create_note("# Durable\n\nkept #keep", DID)
            .unwrap()
            .id
    };
    let store = NoteStore::open_dir(dir.path()).unwrap();
    let loaded = store.get_note(&id).unwrap().expect("note survives reopen");
    assert_eq!(loaded.metadata.title, "Durable");
    assert_eq!(loaded.metadata.tags, vec!["keep"]);
    assert_eq!(loaded.body.as_str(), "# Durable\n\nkept #keep");
    // the search index persisted too — no reindex needed after reopen
    assert_eq!(store.search("kept", 10).unwrap()[0].note_id, id);
}

#[test]
fn opening_a_pre_status_column_database_migrates_it_idempotently() {
    // Simulate an on-disk `kiem.db` from before the `status` column existed:
    // hand-write the old schema (no `status`) and one row, bypassing NoteStore.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("kiem.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE notes (
                id          TEXT PRIMARY KEY,
                title       TEXT NOT NULL,
                tags        TEXT NOT NULL,
                author_did  TEXT NOT NULL,
                note_type   TEXT NOT NULL,
                pinned      INTEGER NOT NULL,
                deleted     INTEGER NOT NULL,
                has_todos   INTEGER NOT NULL,
                created_at  TEXT NOT NULL,
                modified_at TEXT NOT NULL,
                doc         BLOB NOT NULL
            );
            INSERT INTO notes (id, title, tags, author_did, note_type, pinned, deleted,
                has_todos, created_at, modified_at, doc)
            VALUES ('old-1', 'Old note', '[]', 'did:key:z6MkTest', 'note', 0, 0, 0,
                '2026-06-01T00:00:00Z', '2026-06-01T00:00:00Z', x'00');",
        )
        .unwrap();
    }

    // Opening with the current code must migrate the column, not error.
    let store = NoteStore::open(&db_path).unwrap();
    let notes = store.list_notes().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].status, None);

    // Reopening again must be a no-op (idempotent guard), not a second failed
    // `ALTER TABLE ADD COLUMN` on an already-migrated database.
    drop(store);
    let store = NoteStore::open(&db_path).unwrap();
    assert_eq!(store.list_notes().unwrap().len(), 1);
}

#[test]
fn project_todos_aggregate_across_tagged_notes_excluding_others() {
    let store = store_with(&[
        note(
            "a",
            "# A #proj/x\n- [ ] a1\n- [x] done\n- [ ] a2",
            "2026-06-28T10:00:00Z",
        ),
        note("b", "# B #proj/x\n- [ ] b1", "2026-06-28T11:00:00Z"),
        note(
            "c",
            "# C #other\n- [ ] not-in-project",
            "2026-06-28T12:00:00Z",
        ),
    ]);
    let todos = store.list_todo_items_for_tag("proj/x").unwrap();
    // Only the two project notes' *unchecked* items, note-list order (b before a:
    // more recently modified) then document order.
    let texts: Vec<_> = todos.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(texts, vec!["b1", "a1", "a2"]);
    assert!(todos.iter().all(|t| t.note_id == "a" || t.note_id == "b"));
}

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
fn open_todo_items_aggregate_across_all_live_notes() {
    let mut store = store_with(&[
        note(
            "a",
            "# A #proj/x\n- [ ] a1\n- [x] done\n- [ ] a2",
            "2026-06-28T10:00:00Z",
        ),
        note("b", "# B untagged\n- [ ] b1", "2026-06-28T11:00:00Z"),
        note("c", "# C no todos here", "2026-06-28T12:00:00Z"),
        note("d", "# D trashed\n- [ ] gone", "2026-06-28T13:00:00Z"),
    ]);
    store.delete_note("d").unwrap();

    let todos = store.list_open_todo_items().unwrap();
    // Every live note's *unchecked* items regardless of tags, note-list order
    // (most recently modified first) then document order; trashed notes drop out.
    let texts: Vec<_> = todos.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(texts, vec!["b1", "a1", "a2"]);
}

#[test]
fn set_todo_checked_removes_item_from_aggregate_and_keeps_tags() {
    let mut store = store_with(&[note(
        "a",
        "# A #proj/x\n- [ ] keep\n- [ ] finish",
        "2026-06-28T10:00:00Z",
    )]);
    let before = store.list_todo_items_for_tag("proj/x").unwrap();
    assert_eq!(before.len(), 2);

    let meta = store.set_todo_checked("a", 1, true).unwrap();
    assert_eq!(meta.tags, vec!["proj/x"], "tags re-derived intact");

    let after = store.list_todo_items_for_tag("proj/x").unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].text, "keep");
    // the underlying note body actually flipped
    let body = store
        .get_note("a")
        .unwrap()
        .unwrap()
        .body
        .as_str()
        .to_owned();
    assert!(body.contains("- [x] finish"));
}

#[test]
fn set_todos_checked_uses_stable_indices_and_is_atomic() {
    let mut store = store_with(&[note(
        "a",
        "# A\n- [ ] first\n- [ ] second\n- [ ] third",
        "2026-06-28T10:00:00Z",
    )]);

    store.set_todos_checked("a", &[0, 2], true).unwrap();
    let body = store
        .get_note("a")
        .unwrap()
        .unwrap()
        .body
        .as_str()
        .to_owned();
    assert!(body.contains("- [x] first"));
    assert!(body.contains("- [ ] second"));
    assert!(body.contains("- [x] third"));

    let err = store.set_todos_checked("a", &[1, 9], true);
    assert!(matches!(err, Err(StoreError::Document { .. })));
    let unchanged = store
        .get_note("a")
        .unwrap()
        .unwrap()
        .body
        .as_str()
        .to_owned();
    assert_eq!(
        unchanged, body,
        "an invalid batch must not partially persist"
    );
}

#[test]
fn project_todos_empty_when_no_tagged_notes() {
    let store = store_with(&[]);
    assert!(store
        .list_todo_items_for_tag("proj/none")
        .unwrap()
        .is_empty());
}

#[test]
fn set_todo_checked_errors_on_missing_note_or_bad_index() {
    let mut store = store_with(&[note("a", "# A #proj/x\n- [ ] only", "2026-06-28T10:00:00Z")]);
    assert!(matches!(
        store.set_todo_checked("ghost", 0, true),
        Err(StoreError::NotFound(_))
    ));
    // out-of-range index surfaces as a document error
    assert!(matches!(
        store.set_todo_checked("a", 9, true),
        Err(StoreError::Document { .. })
    ));
}

#[test]
fn editing_a_multibyte_body_does_not_corrupt() {
    // The regression: any note whose stored body has a multi-byte char (here
    // "↔" and "café ☕") used to corrupt on the NEXT edit, because the splice
    // was computed in bytes but Automerge indexes text by scalars.
    let mut store = store_with(&[note(
        "m",
        "# Plan\n- [ ] editor ↔ CRDT loop\n- [ ] café ☕ task",
        "2026-06-28T10:00:00Z",
    )]);
    store
        .update_note("m", "# Plan\n- [x] editor ↔ CRDT loop\n- [ ] café ☕ task")
        .unwrap();
    let loaded = store.get_note("m").unwrap().unwrap();
    assert_eq!(
        loaded.body.as_str(),
        "# Plan\n- [x] editor ↔ CRDT loop\n- [ ] café ☕ task"
    );
    // A second edit that rewrites everything after the multibyte chars, too.
    store
        .update_note("m", "# Plan\n- [x] editor ↔ CRDT loop\n- [x] café ☕ DONE")
        .unwrap();
    assert_eq!(
        store.get_note("m").unwrap().unwrap().body.as_str(),
        "# Plan\n- [x] editor ↔ CRDT loop\n- [x] café ☕ DONE"
    );
}

#[test]
fn edit_lines_targets_a_line_and_guards_stale_version() {
    let mut store = store_with(&[note(
        "e",
        "# T #proj/x\n- [ ] a ☕\n- [ ] b\n- [ ] c",
        "2026-06-28T10:00:00Z",
    )]);
    let version = store.note_version("e").unwrap();

    // Replace line 3 ("- [ ] b") only; multibyte line above is untouched.
    store
        .edit_lines("e", Some(&version), 3, 3, "- [x] b")
        .unwrap();
    assert_eq!(
        store.get_note("e").unwrap().unwrap().body.as_str(),
        "# T #proj/x\n- [ ] a ☕\n- [x] b\n- [ ] c"
    );

    // The old version token is now stale → a guarded edit is rejected.
    assert!(matches!(
        store.edit_lines("e", Some(&version), 2, 2, "- [x] a ☕"),
        Err(StoreError::VersionMismatch { .. })
    ));
    // Re-reading the version lets it through.
    let fresh = store.note_version("e").unwrap();
    store
        .edit_lines("e", Some(&fresh), 2, 2, "- [x] a ☕")
        .unwrap();
    assert_eq!(store.list_todo_items_for_tag("proj/x").unwrap().len(), 1);
}

#[test]
fn edit_lines_refuses_to_strip_the_only_tag() {
    let mut store = store_with(&[note(
        "e",
        "# T #proj/x\n- [ ] a\n- [ ] b",
        "2026-06-28T10:00:00Z",
    )]);

    // Replacing every line with tag-free text would leave the note with
    // zero tags — rejected before the write ever lands.
    match store.edit_lines("e", None, 1, 3, "no tags here") {
        Err(StoreError::TagsWouldBeLost { id, tags }) => {
            assert_eq!(id, "e");
            assert_eq!(tags, vec!["proj/x"]);
        }
        other => panic!("expected TagsWouldBeLost, got {other:?}"),
    }

    // The rejected edit must not have mutated the note.
    let loaded = store.get_note("e").unwrap().unwrap();
    assert_eq!(loaded.body.as_str(), "# T #proj/x\n- [ ] a\n- [ ] b");
    assert_eq!(loaded.metadata.tags, vec!["proj/x"]);
}

#[test]
fn explicit_tag_operations_are_idempotent_and_may_leave_a_note_untagged() {
    let mut store = store_with(&[note("a", "# A\n\n#proj/x #keep", "2026-06-28T10:00:00Z")]);

    store.add_tag("a", "keep").unwrap();
    assert_eq!(
        store
            .get_note("a")
            .unwrap()
            .unwrap()
            .body
            .as_str()
            .matches("#keep")
            .count(),
        1
    );

    store.remove_tag("a", "proj/x").unwrap();
    assert_eq!(
        store.get_note("a").unwrap().unwrap().metadata.tags,
        vec!["keep"]
    );
    store.remove_tag("a", "keep").unwrap();
    let note = store.get_note("a").unwrap().unwrap();
    assert!(note.metadata.tags.is_empty());
    assert!(!note.body.as_str().contains("#keep"));
}

#[test]
fn set_note_type_reclassifies_and_persists_to_the_column() {
    let mut store = store_with(&[note("a", "# A\n\n#proj/x", "2026-06-28T10:00:00Z")]);
    assert_eq!(
        store.list_by_tag_and_type("proj/x", "note").unwrap().len(),
        1
    );
    store.set_note_type("a", "plan").unwrap();
    // The denormalized column (what list reads) must reflect the new type.
    assert_eq!(
        store.list_by_tag_and_type("proj/x", "plan").unwrap().len(),
        1
    );
    assert!(store
        .list_by_tag_and_type("proj/x", "note")
        .unwrap()
        .is_empty());
    // Empty resets to the default.
    store.set_note_type("a", "").unwrap();
    assert_eq!(
        store.get_note("a").unwrap().unwrap().metadata.note_type,
        "note"
    );
}

#[test]
fn bulk_error_or_panic_rolls_back_and_leaves_the_store_usable() {
    let mut store = NoteStore::open_in_memory_with_search().unwrap();

    // Error inside the closure: ROLLBACK, and the parked search index restored.
    let err = store
        .bulk(|store| -> Result<(), StoreError> {
            store.create_note("# Doomed", DID)?;
            Err(StoreError::NotFound("boom".into()))
        })
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)));
    assert!(store.list_notes().unwrap().is_empty(), "must roll back");

    // Panic inside the closure: same guarantees, then the panic propagates.
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = store.bulk(|store| -> Result<(), StoreError> {
            store.create_note("# Doomed too", DID)?;
            panic!("boom");
        });
    }));
    assert!(panicked.is_err());
    assert!(store.list_notes().unwrap().is_empty(), "must roll back");

    // No zombie transaction and search survived: a plain write commits and
    // is findable.
    let meta = store.create_note("# Survivor", DID).unwrap();
    assert_eq!(store.search("survivor", 10).unwrap()[0].note_id, meta.id);
}

#[test]
fn delete_note_succeeds_even_when_the_search_index_write_fails() {
    use kiem_core::search::SearchIndex;

    // Hold the tantivy writer lock from a separate process-like instance for
    // comfortably longer than NoteStore's retry budget (~2s, 40 retries x
    // 50ms — see search::WRITER_LOCK_RETRIES/WRITER_LOCK_BACKOFF; 4s gives
    // headroom over scheduler jitter so the budget reliably exhausts),
    // simulating exactly what happened in practice: a CLI command and the
    // GUI app's sync ticker both writing to the same on-disk index at once.
    let dir = tempfile::tempdir().unwrap();
    let search_dir = dir.path().join("search");
    let mut store = NoteStore::open_dir(dir.path()).unwrap();
    let id = store.create_note("# Contended", DID).unwrap().id;

    let contender_dir = search_dir.clone();
    let contender = std::thread::spawn(move || {
        let mut idx = SearchIndex::open_in_dir(&contender_dir).unwrap();
        idx.index_note_deferred(
            &kiem_core::note::NoteMetadata {
                id: "holder".into(),
                title: "Holder".into(),
                tags: vec![],
                author_did: DID.into(),
                note_type: "note".into(),
                pinned: false,
                deleted: false,
                created_at: "2026-06-10T10:00:00Z".into(),
                modified_at: "2026-06-10T10:00:00Z".into(),
                status: None,
            },
            "# Holder",
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(4000));
        idx.flush().unwrap();
    });
    std::thread::sleep(std::time::Duration::from_millis(100));

    // The actual regression: this must return Ok — the note data change is
    // durable in SQLite regardless of the search index being contended —
    // not bubble the index's transient LockBusy up as a hard failure.
    let result = store.delete_note(&id);
    assert!(
        result.is_ok(),
        "delete must succeed even if the search index update is contended: {result:?}"
    );
    assert!(store.get_note(&id).unwrap().unwrap().metadata.deleted);

    contender.join().unwrap();
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
