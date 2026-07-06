//! Read-side store tests: listings, filters, counts, tags, and search.
//! Split from `store_tests.rs` (file-size limit); shares its tiny helpers
//! by duplication rather than a `common` module.

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
fn list_tags_counts_live_notes_only() {
    let mut store = store_with(&[
        note("a", "# A\n\nx #shared #solo", "2026-06-10T10:00:00Z"),
        note("b", "# B\n\ny #shared", "2026-06-10T10:00:01Z"),
        note("c", "# C\n\nz #gone", "2026-06-10T10:00:02Z"),
    ]);
    store.delete_note("c").unwrap();
    assert_eq!(
        store.list_tags().unwrap(),
        vec![("shared".to_owned(), 2), ("solo".to_owned(), 1)]
    );
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
fn note_types_are_settable_and_filterable() {
    let mut store = NoteStore::open_in_memory().unwrap();
    let plan = store
        .create_note_with_type("# Plan A\n\n#proj/x", DID, "plan")
        .unwrap();
    store.create_note_with_type("# Decision\n\n#proj/x", DID, "decision").unwrap();
    store.create_note("# Plain\n\n#proj/x", DID).unwrap(); // defaults to "note"

    assert_eq!(plan.note_type, "plan");
    let plans = store.list_by_tag_and_type("proj/x", "plan").unwrap();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].title, "Plan A");
    // Default type still lists under "note".
    assert_eq!(store.list_by_tag_and_type("proj/x", "note").unwrap().len(), 1);
    // All three are still in the untyped project view.
    assert_eq!(store.list_by_tag("proj/x").unwrap().len(), 3);
    // An empty type falls back to the default.
    let m = store.create_note_with_type("# E\n\n#proj/x", DID, "  ").unwrap();
    assert_eq!(m.note_type, "note");
}

#[test]
fn filter_counts_match_the_list_queries() {
    let mut store = NoteStore::open_in_memory().unwrap();
    store.create_note("# Todo note\n\n- [ ] open item #x", DID).unwrap(); // todo + today + tagged
    store.create_note("# Untagged, modified today", DID).unwrap();
    let pinned = store.create_note("# Pinned #x", DID).unwrap();
    store.set_pinned(&pinned.id, true).unwrap();
    let dead = store.create_note("# Trashed", DID).unwrap();
    store.delete_note(&dead.id).unwrap();
    // A yesterday-dated note: today must not count it.
    store
        .insert_note(&note("old", "# Old", "2026-06-28T10:00:00Z"))
        .unwrap();

    let counts = store.filter_counts().unwrap();
    assert_eq!(counts.todo as usize, store.list_todos().unwrap().len());
    assert_eq!(counts.today as usize, store.list_today().unwrap().len());
    assert_eq!(counts.untagged as usize, store.list_untagged().unwrap().len());
    assert_eq!(counts.pinned as usize, store.list_pinned().unwrap().len());
    assert_eq!(counts.trash as usize, store.list_deleted().unwrap().len());
    // Sanity: the seeded store actually exercises every bucket.
    assert!(counts.todo >= 1 && counts.today >= 2 && counts.untagged >= 1);
    assert_eq!(counts.pinned, 1);
    assert_eq!(counts.trash, 1);
}
