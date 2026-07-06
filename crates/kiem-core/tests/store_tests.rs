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
    let meta = store.create_note("# Hello\n\nWorld #greeting", DID).unwrap();
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
    let meta = store.update_note("a", "# After\n\n- [ ] task #later").unwrap();
    assert_eq!(meta.title, "After");
    assert_eq!(meta.tags, vec!["later"]);
    assert_eq!(meta.created_at, "2026-06-10T10:00:00Z");
    assert!(meta.modified_at > meta.created_at, "modified_at must advance");

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

    let meta = store.update_note("a", "---\nstatus: active\n---\n# After #tag").unwrap();
    assert_eq!(meta.status, Some("active".to_string()));

    let meta = store.update_note("a", "---\nstatus: completed\n---\n# After #tag").unwrap();
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
        store.create_note("# Durable\n\nkept #keep", DID).unwrap().id
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
        note("a", "# A #proj/x\n- [ ] a1\n- [x] done\n- [ ] a2", "2026-06-28T10:00:00Z"),
        note("b", "# B #proj/x\n- [ ] b1", "2026-06-28T11:00:00Z"),
        note("c", "# C #other\n- [ ] not-in-project", "2026-06-28T12:00:00Z"),
    ]);
    let todos = store.list_todo_items_for_tag("proj/x").unwrap();
    // Only the two project notes' *unchecked* items, note-list order (b before a:
    // more recently modified) then document order.
    let texts: Vec<_> = todos.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(texts, vec!["b1", "a1", "a2"]);
    assert!(todos.iter().all(|t| t.note_id == "a" || t.note_id == "b"));
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
    let body = store.get_note("a").unwrap().unwrap().body.as_str().to_owned();
    assert!(body.contains("- [x] finish"));
}

#[test]
fn project_todos_empty_when_no_tagged_notes() {
    let store = store_with(&[]);
    assert!(store.list_todo_items_for_tag("proj/none").unwrap().is_empty());
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
    assert_eq!(loaded.body.as_str(), "# Plan\n- [x] editor ↔ CRDT loop\n- [ ] café ☕ task");
    // A second edit that rewrites everything after the multibyte chars, too.
    store.update_note("m", "# Plan\n- [x] editor ↔ CRDT loop\n- [x] café ☕ DONE").unwrap();
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
    store.edit_lines("e", Some(&version), 3, 3, "- [x] b").unwrap();
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
    store.edit_lines("e", Some(&fresh), 2, 2, "- [x] a ☕").unwrap();
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
fn set_note_type_reclassifies_and_persists_to_the_column() {
    let mut store = store_with(&[note("a", "# A\n\n#proj/x", "2026-06-28T10:00:00Z")]);
    assert_eq!(store.list_by_tag_and_type("proj/x", "note").unwrap().len(), 1);
    store.set_note_type("a", "plan").unwrap();
    // The denormalized column (what list reads) must reflect the new type.
    assert_eq!(store.list_by_tag_and_type("proj/x", "plan").unwrap().len(), 1);
    assert!(store.list_by_tag_and_type("proj/x", "note").unwrap().is_empty());
    // Empty resets to the default.
    store.set_note_type("a", "").unwrap();
    assert_eq!(store.get_note("a").unwrap().unwrap().metadata.note_type, "note");
}
