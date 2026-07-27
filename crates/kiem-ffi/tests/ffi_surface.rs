//! The FFI surface as Swift sees it: every test drives only \`KiemStore\`'s
//! exported methods and the \`PeerEvents\`/\`TransferProgress\` callback traits.
//!
//! An integration test on purpose — if something here needs \`pub(crate)\`
//! access, that is a sign the bridge grew a seam Swift cannot reach, which is
//! worth noticing.

use std::sync::Arc;

use kiem_ffi::*;

fn open_temp() -> (tempfile::TempDir, KiemStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = KiemStore::open(dir.path().to_string_lossy().into_owned()).unwrap();
    (dir, store)
}

#[test]
fn crud_and_search_through_the_ffi_surface() {
    let (_dir, store) = open_temp();
    let meta = store
        .create_note("# Bridge\n\nhello #ffi".into(), "did:key:test".into())
        .unwrap();
    assert_eq!(meta.title, "Bridge");
    assert_eq!(meta.tags, vec!["ffi"]);

    let note = store.get_note(meta.id.clone()).unwrap().expect("exists");
    assert_eq!(note.body, "# Bridge\n\nhello #ffi");

    assert_eq!(store.list_notes().unwrap().len(), 1);
    assert_eq!(
        store.search("hello".into(), 10).unwrap()[0].note_id,
        meta.id
    );
    assert_eq!(store.get_tags().unwrap()[0].tag, "ffi");

    store.delete_note(meta.id.clone()).unwrap();
    assert!(store.list_notes().unwrap().is_empty());
    assert_eq!(store.list_deleted().unwrap().len(), 1);
}

#[test]
fn project_todos_aggregate_and_toggle_through_the_ffi_surface() {
    let (_dir, store) = open_temp();
    let note = store
        .create_note(
            "# Tasks #proj/demo\n- [ ] a\n- [ ] b".into(),
            "did:key:test".into(),
        )
        .unwrap();

    let todos = store.list_todo_items_for_tag("proj/demo".into()).unwrap();
    assert_eq!(todos.len(), 2);
    assert_eq!(todos[0].note_id, note.id);
    assert_eq!(todos[0].index, 0);

    store.set_todo_checked(note.id.clone(), 0, true).unwrap();
    let after = store.list_todo_items_for_tag("proj/demo".into()).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].text, "b");

    store
        .set_todo_text(note.id.clone(), 1, "b renamed".into())
        .unwrap();
    let renamed = store.list_todo_items_for_tag("proj/demo".into()).unwrap();
    assert_eq!(renamed.len(), 1);
    assert_eq!(renamed[0].text, "b renamed");
}

/// Progress relay stub: transfers need one; these tests assert counts only.
struct NoProgress;
impl TransferProgress for NoProgress {
    fn on_progress(&self, _done: u32, _total: u32) {}
}

#[test]
fn notes_export_and_import_through_the_ffi_surface() {
    let (_dir, store) = open_temp();
    store
        .create_note("# A\n\n- [ ] alpha\n\n#proj/demo".into(), "t".into())
        .unwrap();
    store.create_note("# Unfiled".into(), "t".into()).unwrap();

    let out = tempfile::tempdir().unwrap();
    let dir = out.path().to_string_lossy().into_owned();
    let exported = store
        .export_notes(dir.clone(), Arc::new(NoProgress))
        .unwrap();
    assert_eq!((exported.transferred, exported.skipped), (1, 1));

    let (_dir2, fresh) = open_temp();
    let imported = fresh
        .import_notes(dir.clone(), "t".into(), true, Arc::new(NoProgress))
        .unwrap();
    assert_eq!((imported.transferred, imported.skipped), (1, 0));
    assert_eq!(fresh.list_by_tag("proj/demo".into()).unwrap()[0].title, "A");
    // Re-import is a no-op.
    let again = fresh
        .import_notes(dir, "t".into(), true, Arc::new(NoProgress))
        .unwrap();
    assert_eq!((again.transferred, again.skipped), (0, 1));
}

#[test]
fn folders_as_projects_flag_routes_to_the_right_import_mode() {
    // A file with NO inline tag, so the only possible proj/* tag is the
    // one the Folders mode mints from the directory name — this is what
    // actually discriminates the bool (the round-trip test's bodies carry
    // their tags inline and pass under either mode).
    let dump = tempfile::tempdir().unwrap();
    std::fs::write(dump.path().join("plain.md"), "# Plain").unwrap();
    let dir = dump.path().to_string_lossy().into_owned();

    let (_d1, flat) = open_temp();
    flat.import_notes(dir.clone(), "t".into(), false, Arc::new(NoProgress))
        .unwrap();
    assert!(
        flat.get_tags()
            .unwrap()
            .iter()
            .all(|t| !t.tag.starts_with("proj/")),
        "false must not mint a project"
    );

    let (_d2, foldered) = open_temp();
    foldered
        .import_notes(dir, "t".into(), true, Arc::new(NoProgress))
        .unwrap();
    assert!(
        foldered
            .get_tags()
            .unwrap()
            .iter()
            .any(|t| t.tag.starts_with("proj/")),
        "true must mint a project from the folder"
    );
}

#[test]
fn transfer_errors_map_to_the_transfer_variant() {
    let (_dir, store) = open_temp();
    match store.import_notes(
        "/nonexistent-kiem-import-dir".into(),
        "t".into(),
        true,
        Arc::new(NoProgress),
    ) {
        Err(KiemError::Transfer { message }) => {
            assert!(
                message.contains("resolving directory"),
                "unhelpful message: {message}"
            );
        }
        other => panic!("expected Transfer, got {other:?}"),
    }
}

#[test]
fn not_found_maps_to_typed_error() {
    let (_dir, store) = open_temp();
    match store.update_note("ghost".into(), "x".into()) {
        Err(KiemError::NotFound { id }) => assert_eq!(id, "ghost"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn concurrent_edits_and_sync_receives_serialize() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(KiemStore::open(dir.path().to_string_lossy().into_owned()).unwrap());
    let meta = store
        .create_note("# Threads".into(), "did:t".into())
        .unwrap();

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let store = store.clone();
            let id = meta.id.clone();
            std::thread::spawn(move || {
                store
                    .update_note(id, format!("# Threads\n\nedit {i}"))
                    .unwrap();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert!(store
        .get_note(meta.id)
        .unwrap()
        .unwrap()
        .body
        .contains("edit"));
}

struct NullEvents;
impl PeerEvents for NullEvents {
    fn on_connected(&self, _peer_id: String) {}
    fn on_disconnected(&self, _peer_id: String) {}
    fn on_sync_activity(&self, _peer_id: String) {}
    fn approve_pairing(&self, _peer_id: String) -> bool {
        true
    }
}

#[test]
fn two_stores_sync_over_a_real_iroh_mesh() {
    let (_dir_a, a) = open_temp();
    let (_dir_b, b) = open_temp();

    a.create_note("# Mesh\n\nvia ffi sync".into(), "did:a".into())
        .unwrap();

    a.start_sync(50, Arc::new(NullEvents)).unwrap();
    b.start_sync(50, Arc::new(NullEvents)).unwrap();
    a.arm_pairing(60);
    b.arm_pairing(60);

    // Fetch live tickets and add after sync starts: the app follows this
    // route, so both calls must enter the mesh's existing Tokio runtime.
    let ticket_a = a.pair_ticket().unwrap();
    let ticket_b = b.pair_ticket().unwrap();
    a.add_known_peer(ticket_b).unwrap();
    b.add_known_peer(ticket_a).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    loop {
        if b.list_notes().unwrap().iter().any(|n| n.title == "Mesh") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "note never synced over the FFI mesh"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    assert_eq!(a.connected_peers().len(), 1);
    assert_eq!(b.connected_peers().len(), 1);

    a.stop_sync();
    b.stop_sync();
}
