//! Kiem FFI: thin UniFFI bridge exposing `kiem-core` to Swift.
//!
//! `KiemStore` is an opaque handle; Automerge documents never cross the
//! boundary — only value records ([`NoteMetadata`], [`Note`],
//! [`SearchResult`]) do. Sync runs entirely in Rust (`kiem-sync`'s iroh
//! mesh, started via [`KiemStore::start_sync`]); Swift only hears peer
//! connect/disconnect events. One `Mutex` guards the store *and* the sync
//! engine together so a local edit can never interleave with a sync
//! receive mid-hydrate/reconcile (autosurgeon `StaleHeads`).
//!
//! The mirrored record types exist because `kiem-core` stays FFI-free by
//! design; `From` impls keep the mapping mechanical.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiem_core::store::NoteStore;
use kiem_core::sync::SyncEngine;
use kiem_sync::SharedState;

uniffi::setup_scaffolding!();

mod records;
pub use records::*;

/// Hears `(done, total)` after each note/file of an export/import, so the
/// app can draw a determinate progress bar. Called on the transfer's thread.
#[uniffi::export(with_foreign)]
pub trait TransferProgress: Send + Sync {
    fn on_progress(&self, done: u32, total: u32);
}

/// Forwarded to Swift as sync mesh peers connect/disconnect, and to ask the
/// user to approve an incoming pairing.
#[uniffi::export(with_foreign)]
pub trait PeerEvents: Send + Sync {
    fn on_connected(&self, peer_id: String);
    fn on_disconnected(&self, peer_id: String);
    /// An unknown peer dialed in during an open pairing window — return true to
    /// trust it. Called on a blocking thread, so the Swift side may wait on a
    /// user prompt (e.g. a semaphore released by an Allow/Deny sheet).
    fn approve_pairing(&self, peer_id: String) -> bool;
}

struct EventsAdapter(Arc<dyn PeerEvents>);

impl kiem_sync::MeshEvents for EventsAdapter {
    fn on_connected(&self, peer: kiem_sync::EndpointId) {
        self.0.on_connected(peer.to_string());
    }
    fn on_disconnected(&self, peer: kiem_sync::EndpointId) {
        self.0.on_disconnected(peer.to_string());
    }
    fn on_error(&self, context: &str, error: &str) {
        eprintln!("kiem sync: {context}: {error}");
    }
    fn approve_pairing(&self, peer: kiem_sync::EndpointId) -> bool {
        self.0.approve_pairing(peer.to_string())
    }
}

struct SyncHandle {
    runtime: tokio::runtime::Runtime,
    mesh: Arc<kiem_sync::Mesh>,
}

#[derive(uniffi::Object)]
pub struct KiemStore {
    data_dir: PathBuf,
    state: SharedState,
    sync: Mutex<Option<SyncHandle>>,
}

impl KiemStore {
    /// Run `f` with the store+engine lock held for the whole operation.
    fn with<T>(
        &self,
        f: impl FnOnce(&mut NoteStore, &mut SyncEngine) -> Result<T, KiemError>,
    ) -> Result<T, KiemError> {
        let mut guard = self.state.lock().expect("KiemStore lock poisoned");
        let (store, engine) = &mut *guard;
        f(store, engine)
    }
}

#[uniffi::export]
impl KiemStore {
    /// Open (or create) the data directory: `kiem.db` + search index.
    #[uniffi::constructor]
    pub fn open(data_dir: String) -> Result<Self, KiemError> {
        let data_dir = PathBuf::from(data_dir);
        let store = NoteStore::open_dir(&data_dir)?;
        Ok(KiemStore {
            data_dir,
            state: Arc::new(Mutex::new((store, SyncEngine::new()))),
            sync: Mutex::new(None),
        })
    }

    pub fn create_note(&self, body: String, author_did: String) -> Result<NoteMetadata, KiemError> {
        self.with(|store, _| Ok(store.create_note(&body, &author_did)?.into()))
    }

    pub fn get_note(&self, id: String) -> Result<Option<Note>, KiemError> {
        self.with(|store, _| {
            Ok(store.get_note(&id)?.map(|doc| Note {
                body: doc.body.as_str().to_owned(),
                metadata: doc.metadata.into(),
            }))
        })
    }

    /// Holds the lock for the full hydrate→edit→reconcile cycle so an
    /// incoming sync message cannot interleave (autosurgeon StaleHeads).
    pub fn update_note(&self, id: String, body: String) -> Result<NoteMetadata, KiemError> {
        self.with(|store, _| Ok(store.update_note(&id, &body)?.into()))
    }

    pub fn set_pinned(&self, id: String, pinned: bool) -> Result<NoteMetadata, KiemError> {
        self.with(|store, _| Ok(store.set_pinned(&id, pinned)?.into()))
    }

    pub fn delete_note(&self, id: String) -> Result<NoteMetadata, KiemError> {
        self.with(|store, _| Ok(store.delete_note(&id)?.into()))
    }

    /// Add a body-derived hashtag (no leading `#`) — the app's drag-to-tag
    /// and add-to-project actions. No-op if the body already carries it.
    pub fn add_tag(&self, id: String, tag: String) -> Result<NoteMetadata, KiemError> {
        self.with(|store, _| Ok(store.add_tag(&id, &tag)?.into()))
    }

    pub fn restore_note(&self, id: String) -> Result<NoteMetadata, KiemError> {
        self.with(|store, _| Ok(store.restore_note(&id)?.into()))
    }

    pub fn list_notes(&self) -> Result<Vec<NoteMetadata>, KiemError> {
        self.with(|store, _| Ok(into_meta(store.list_notes()?)))
    }

    pub fn list_by_tag(&self, tag: String) -> Result<Vec<NoteMetadata>, KiemError> {
        self.with(|store, _| Ok(into_meta(store.list_by_tag(&tag)?)))
    }

    pub fn list_todos(&self) -> Result<Vec<NoteMetadata>, KiemError> {
        self.with(|store, _| Ok(into_meta(store.list_todos()?)))
    }

    pub fn list_today(&self) -> Result<Vec<NoteMetadata>, KiemError> {
        self.with(|store, _| Ok(into_meta(store.list_today()?)))
    }

    pub fn list_untagged(&self) -> Result<Vec<NoteMetadata>, KiemError> {
        self.with(|store, _| Ok(into_meta(store.list_untagged()?)))
    }

    pub fn list_pinned(&self) -> Result<Vec<NoteMetadata>, KiemError> {
        self.with(|store, _| Ok(into_meta(store.list_pinned()?)))
    }

    pub fn list_deleted(&self) -> Result<Vec<NoteMetadata>, KiemError> {
        self.with(|store, _| Ok(into_meta(store.list_deleted()?)))
    }

    /// Smart-filter match counts for the sidebar (one scan, no lists).
    pub fn filter_counts(&self) -> Result<FilterCounts, KiemError> {
        self.with(|store, _| Ok(store.filter_counts()?.into()))
    }

    pub fn get_tags(&self) -> Result<Vec<TagCount>, KiemError> {
        self.with(|store, _| {
            Ok(store
                .list_tags()?
                .into_iter()
                .map(|(tag, count)| TagCount {
                    tag,
                    count: count as u64,
                })
                .collect())
        })
    }

    /// All unchecked todo items across the project's notes (by tag), each with a
    /// (note_id, index) address.
    pub fn list_todo_items_for_tag(&self, tag: String) -> Result<Vec<ProjectTodo>, KiemError> {
        self.with(|store, _| {
            Ok(store
                .list_todo_items_for_tag(&tag)?
                .into_iter()
                .map(Into::into)
                .collect())
        })
    }

    /// All unchecked todo items across every live note (the Todo smart filter),
    /// same shape and ordering as `list_todo_items_for_tag`.
    pub fn list_open_todo_items(&self) -> Result<Vec<ProjectTodo>, KiemError> {
        self.with(|store, _| {
            Ok(store
                .list_open_todo_items()?
                .into_iter()
                .map(Into::into)
                .collect())
        })
    }

    /// Permanently erase every trashed note (Empty Trash). Purged ids are
    /// tombstoned so sync cannot resurrect them. Returns the erased count.
    pub fn purge_deleted(&self) -> Result<u32, KiemError> {
        self.with(|store, _| Ok(store.purge_deleted()? as u32))
    }

    /// Permanently erase a project: every note carrying `tag` (trashed ones
    /// included), tombstoned like `purge_deleted`. Returns the erased count.
    pub fn purge_tag(&self, tag: String) -> Result<u32, KiemError> {
        self.with(|store, _| Ok(store.purge_tag(&tag)? as u32))
    }

    /// Toggle the checkbox at `index` within note `note_id`, persisting the edit.
    pub fn set_todo_checked(
        &self,
        note_id: String,
        index: u32,
        checked: bool,
    ) -> Result<NoteMetadata, KiemError> {
        self.with(|store, _| {
            Ok(store
                .set_todo_checked(&note_id, index as usize, checked)?
                .into())
        })
    }

    /// Replace the text of the todo at `index` within note `note_id`, persisting the edit.
    pub fn set_todo_text(
        &self,
        note_id: String,
        index: u32,
        text: String,
    ) -> Result<NoteMetadata, KiemError> {
        self.with(|store, _| Ok(store.set_todo_text(&note_id, index as usize, &text)?.into()))
    }

    /// Export every project's notes as Markdown files under `dir` — one
    /// folder per project, one file per note. Returns written / skipped
    /// (notes without a project) counts; `progress` hears `(done, total)`
    /// after each note.
    pub fn export_notes(
        &self,
        dir: String,
        progress: Arc<dyn TransferProgress>,
    ) -> Result<TransferSummary, KiemError> {
        self.with(|store, _| {
            let (written, skipped) = kiem_core::transfer::export_all_with_progress(
                store,
                std::path::Path::new(&dir),
                &mut |done, total| progress.on_progress(done as u32, total as u32),
            )?;
            Ok(TransferSummary {
                transferred: written as u32,
                skipped: skipped as u32,
            })
        })
    }

    /// Import a directory of Markdown files as notes. With
    /// `folders_as_projects`, a folder is a project (subfolders each, or the
    /// flat folder itself); without it no project is assigned — notes keep
    /// only the tags already in their bodies. Returns created / skipped
    /// (already-present duplicates) counts; `progress` hears `(done, total)`
    /// after each file.
    pub fn import_notes(
        &self,
        dir: String,
        author_did: String,
        folders_as_projects: bool,
        progress: Arc<dyn TransferProgress>,
    ) -> Result<TransferSummary, KiemError> {
        let source = if folders_as_projects {
            kiem_core::transfer::ProjectSource::Folders
        } else {
            kiem_core::transfer::ProjectSource::None
        };
        self.with(|store, _| {
            let (created, skipped) = kiem_core::transfer::import_with_progress(
                store,
                std::path::Path::new(&dir),
                &author_did,
                source,
                &mut |done, total| progress.on_progress(done as u32, total as u32),
            )?;
            Ok(TransferSummary {
                transferred: created.len() as u32,
                skipped: skipped as u32,
            })
        })
    }

    pub fn search(&self, query: String, limit: u32) -> Result<Vec<SearchResult>, KiemError> {
        self.with(|store, _| {
            Ok(store
                .search(&query, limit as usize)?
                .into_iter()
                .map(Into::into)
                .collect())
        })
    }

    pub fn rebuild_search_index(&self) -> Result<(), KiemError> {
        self.with(|store, _| Ok(store.rebuild_search_index()?))
    }

    // -- P2P sync mesh (kiem-sync / iroh) --

    /// This device's stable identity (its iroh `EndpointId`, hex) — the id
    /// peers see on the mesh, and the value to pass as `author_did` when
    /// creating notes. Created on first use, persisted in the data dir.
    pub fn device_did(&self) -> Result<String, KiemError> {
        Ok(kiem_sync::device_id(&self.data_dir)
            .map_err(sync_err)?
            .to_string())
    }

    /// Binds this device's identity, accepts incoming connections, and dials
    /// every known peer. No-op if already running.
    pub fn start_sync(
        &self,
        interval_ms: u64,
        events: Arc<dyn PeerEvents>,
    ) -> Result<(), KiemError> {
        let mut sync = self.sync.lock().expect("sync lock poisoned");
        if sync.is_some() {
            return Ok(());
        }
        let runtime = tokio::runtime::Runtime::new().map_err(sync_err)?;
        let mesh = runtime
            .block_on(kiem_sync::Mesh::start(
                self.data_dir.clone(),
                self.state.clone(),
                Duration::from_millis(interval_ms.max(100)),
                Arc::new(EventsAdapter(events)),
            ))
            .map_err(sync_err)?;
        *sync = Some(SyncHandle { runtime, mesh });
        Ok(())
    }

    /// Stops the sync mesh. No-op if not running.
    pub fn stop_sync(&self) {
        if let Some(handle) = self.sync.lock().expect("sync lock poisoned").take() {
            handle.runtime.shutdown_background();
        }
    }

    /// This device's shareable pairing ticket, with a relay hint so the peer's
    /// first connect goes through the relay instead of paying cold discovery.
    /// When sync is running it's the live mesh endpoint's ticket; otherwise a
    /// standalone one. Both wait (bounded) for relay registration, so call this
    /// off the main thread.
    pub fn pair_ticket(&self) -> Result<String, KiemError> {
        // A running mesh's futures must run on the runtime that owns it.
        // Clone its handle so the relay wait doesn't hold the sync lock.
        let runtime = self
            .sync
            .lock()
            .expect("sync lock poisoned")
            .as_ref()
            .map(|handle| (handle.runtime.handle().clone(), handle.mesh.clone()));
        match runtime {
            Some((runtime, mesh)) => Ok(runtime.block_on(mesh.ticket_online())),
            None => tokio::runtime::Runtime::new()
                .map_err(sync_err)?
                .block_on(kiem_sync::pair_ticket(&self.data_dir))
                .map_err(sync_err),
        }
    }

    /// Opens the single-use pairing window for `window_secs`, during which one
    /// unknown peer may connect and (after approval) be trusted. No-op if sync
    /// isn't running.
    pub fn arm_pairing(&self, window_secs: u64) {
        if let Some(handle) = self.sync.lock().expect("sync lock poisoned").as_ref() {
            handle
                .mesh
                .arm_pairing(std::time::Duration::from_secs(window_secs));
        }
    }

    /// Whole seconds left on the open pairing window (rounded up), or `None`
    /// when closed — drives the app's countdown.
    pub fn pairing_window_remaining(&self) -> Option<u64> {
        let handle = self.sync.lock().expect("sync lock poisoned");
        handle
            .as_ref()?
            .mesh
            .pairing_window_remaining()
            .map(|d| d.as_secs() + u64::from(d.subsec_nanos() > 0))
    }

    /// Trusts the device behind a pasted/scanned ticket. If sync is running,
    /// forces an immediate pairing dial (bypassing the smaller-id-dials guard so
    /// it connects regardless of id ordering) and also starts the steady-state
    /// dial loop for ongoing reconnection.
    pub fn add_known_peer(&self, ticket: String) -> Result<String, KiemError> {
        let addr = kiem_sync::pair_add(&self.data_dir, &ticket).map_err(sync_err)?;
        let id = addr.id;
        if let Some(handle) = self.sync.lock().expect("sync lock poisoned").as_ref() {
            let _runtime = handle.runtime.enter();
            handle.mesh.pair_dial(addr.clone());
            handle.mesh.dial(addr);
        }
        Ok(id.to_string())
    }

    /// Ids of every paired device (the known-peers file), whether or not it
    /// is currently reachable — the denominator for the sync-status UI.
    pub fn known_peers(&self) -> Result<Vec<String>, KiemError> {
        let peers = kiem_sync::KnownPeers::load(&self.data_dir.join(kiem_sync::PEERS_FILE))
            .map_err(sync_err)?;
        Ok(peers.ids().into_iter().map(|id| id.to_string()).collect())
    }

    /// Currently-connected peer ids, or empty if sync isn't running.
    pub fn connected_peers(&self) -> Vec<String> {
        match self.sync.lock().expect("sync lock poisoned").as_ref() {
            Some(handle) => handle
                .mesh
                .connected_ids()
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
            None => Vec::new(),
        }
    }
}

fn sync_err(err: impl std::fmt::Display) -> KiemError {
    KiemError::Sync {
        message: err.to_string(),
    }
}

fn into_meta(metas: Vec<kiem_core::note::NoteMetadata>) -> Vec<NoteMetadata> {
    metas.into_iter().map(Into::into).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        use std::sync::Arc;
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
}
