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
fn list_orders_by_modified_at_desc_and_skips_deleted() {
    let mut store = store_with(&[
        note("old", "# Old", "2026-06-10T10:00:00Z"),
        note("new", "# New", "2026-06-12T10:00:00Z"),
        note("mid", "# Mid", "2026-06-11T10:00:00Z"),
    ]);
    store.delete_note("mid").unwrap();
    let ids: Vec<String> = store.list_notes().unwrap().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, vec!["new", "old"]);
}

#[test]
fn list_returns_ten_notes_in_modified_order() {
    let notes: Vec<NoteDoc> = (0..10)
        .map(|i| note(&format!("n{i}"), "# N", &format!("2026-06-12T10:00:{i:02}Z")))
        .collect();
    let store = store_with(&notes);
    let listed = store.list_notes().unwrap();
    assert_eq!(listed.len(), 10);
    assert_eq!(listed.first().unwrap().id, "n9");
    assert_eq!(listed.last().unwrap().id, "n0");
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
fn tag_filter_matches_exactly_not_by_prefix() {
    let store = store_with(&[
        note("w", "# W\n\nabout #work", "2026-06-10T10:00:00Z"),
        note("wm", "# WM\n\nabout #work/meetings", "2026-06-10T10:00:01Z"),
        note("none", "# None", "2026-06-10T10:00:02Z"),
    ]);
    let ids: Vec<String> = store.list_by_tag("work").unwrap().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, vec!["w"]);
    let ids: Vec<String> = store.list_by_tag("work/meetings").unwrap().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, vec!["wm"]);
    assert!(store.list_by_tag("absent").unwrap().is_empty());
}

#[test]
fn smart_filters_todo_untagged_pinned_today() {
    let mut store = store_with(&[
        note("todo", "# T\n\n- [ ] open", "2026-06-10T10:00:00Z"),
        note("done", "# D\n\n- [x] closed #done", "2026-06-11T10:00:00Z"),
    ]);
    assert_eq!(store.list_todos().unwrap()[0].id, "todo");
    assert_eq!(store.list_untagged().unwrap().len(), 2 - 1); // "done" has a tag

    store.set_pinned("done", true).unwrap();
    assert_eq!(store.list_pinned().unwrap()[0].id, "done");

    assert_eq!(store.list_modified_on("2026-06-10").unwrap()[0].id, "todo");
    assert!(store.list_modified_on("2026-01-01").unwrap().is_empty());

    let mut fresh = NoteStore::open_in_memory().unwrap();
    let meta = fresh.create_note("# Now", DID).unwrap();
    assert_eq!(fresh.list_today().unwrap()[0].id, meta.id);
}

#[test]
fn search_is_integrated_with_writes() {
    let mut store = NoteStore::open_in_memory_with_search().unwrap();
    let a = store.create_note("# Alpha\n\nthe walrus rests", DID).unwrap();
    let b = store.create_note("# Beta\n\nabout #gardening", DID).unwrap();

    // create → searchable, with metadata flowing through
    let hits = store.search("walrus", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].note_id, a.id);
    assert_eq!(hits[0].title, "Alpha");

    // tags are searchable
    assert_eq!(store.search("gardening", 10).unwrap()[0].note_id, b.id);

    // update → old content gone, new content found
    store.update_note(&a.id, "# Alpha\n\nthe narwhal swims").unwrap();
    assert!(store.search("walrus", 10).unwrap().is_empty());
    assert_eq!(store.search("narwhal", 10).unwrap()[0].note_id, a.id);

    // soft delete → out of results; restore → back
    store.delete_note(&a.id).unwrap();
    assert!(store.search("narwhal", 10).unwrap().is_empty());
    store.restore_note(&a.id).unwrap();
    assert_eq!(store.search("narwhal", 10).unwrap().len(), 1);
}

#[test]
fn search_on_searchless_store_is_an_explicit_error() {
    let store = store_with(&[]);
    assert!(matches!(
        store.search("anything", 10),
        Err(StoreError::SearchDisabled)
    ));
}

#[test]
fn rebuild_restores_a_wiped_index() {
    let mut store = NoteStore::open_in_memory_with_search().unwrap();
    store.create_note("# Kept\n\nfindable zebra", DID).unwrap();
    let trashed = store.create_note("# Trash\n\nhidden okapi", DID).unwrap();
    store.delete_note(&trashed.id).unwrap();

    store.rebuild_search_index().unwrap();
    assert_eq!(store.search("zebra", 10).unwrap().len(), 1);
    assert!(store.search("okapi", 10).unwrap().is_empty(), "deleted notes stay out");
}

#[test]
fn search_scales_to_a_hundred_notes() {
    let mut store = NoteStore::open_in_memory_with_search().unwrap();
    for i in 0..100 {
        store
            .create_note(&format!("# Note {i}\n\nfiller text number{i}"), DID)
            .unwrap();
    }
    let hits = store.search("number42", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, "Note 42");
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
