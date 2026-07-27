//! Checkbox aggregation and mutation: the project todo panel's data, and the
//! stable-index rule that lets checking one item leave the others addressable.

use kiem_core::store::StoreError;

mod common;
use common::{note, store_with};

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
