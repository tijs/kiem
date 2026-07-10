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

/// Forwarded to Swift as sync mesh peers connect/disconnect.
#[uniffi::export(with_foreign)]
pub trait PeerEvents: Send + Sync {
    fn on_connected(&self, peer_id: String);
    fn on_disconnected(&self, peer_id: String);
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
                .map(|(tag, count)| TagCount { tag, count: count as u64 })
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

    /// Toggle the checkbox at `index` within note `note_id`, persisting the edit.
    pub fn set_todo_checked(
        &self,
        note_id: String,
        index: u32,
        checked: bool,
    ) -> Result<NoteMetadata, KiemError> {
        self.with(|store, _| Ok(store.set_todo_checked(&note_id, index as usize, checked)?.into()))
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
        Ok(kiem_sync::device_id(&self.data_dir).map_err(sync_err)?.to_string())
    }

    /// Binds this device's identity, accepts incoming connections, and dials
    /// every known peer. No-op if already running.
    pub fn start_sync(&self, interval_ms: u64, events: Arc<dyn PeerEvents>) -> Result<(), KiemError> {
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

    /// This device's shareable pairing ticket.
    pub fn pair_ticket(&self) -> Result<String, KiemError> {
        tokio::runtime::Runtime::new()
            .map_err(sync_err)?
            .block_on(kiem_sync::pair_ticket(&self.data_dir))
            .map_err(sync_err)
    }

    /// Trusts the device behind a pasted/scanned ticket, dialing it right
    /// away if sync is already running.
    pub fn add_known_peer(&self, ticket: String) -> Result<String, KiemError> {
        let addr = kiem_sync::pair_add(&self.data_dir, &ticket).map_err(sync_err)?;
        let id = addr.id;
        if let Some(handle) = self.sync.lock().expect("sync lock poisoned").as_ref() {
            handle.mesh.dial(addr);
        }
        Ok(id.to_string())
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
    KiemError::Sync { message: err.to_string() }
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
        assert_eq!(store.search("hello".into(), 10).unwrap()[0].note_id, meta.id);
        assert_eq!(store.get_tags().unwrap()[0].tag, "ffi");

        store.delete_note(meta.id.clone()).unwrap();
        assert!(store.list_notes().unwrap().is_empty());
        assert_eq!(store.list_deleted().unwrap().len(), 1);
    }

    #[test]
    fn project_todos_aggregate_and_toggle_through_the_ffi_surface() {
        let (_dir, store) = open_temp();
        let note = store
            .create_note("# Tasks #proj/demo\n- [ ] a\n- [ ] b".into(), "did:key:test".into())
            .unwrap();

        let todos = store.list_todo_items_for_tag("proj/demo".into()).unwrap();
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0].note_id, note.id);
        assert_eq!(todos[0].index, 0);

        store.set_todo_checked(note.id.clone(), 0, true).unwrap();
        let after = store.list_todo_items_for_tag("proj/demo".into()).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].text, "b");
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
        let meta = store.create_note("# Threads".into(), "did:t".into()).unwrap();

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let store = store.clone();
                let id = meta.id.clone();
                std::thread::spawn(move || {
                    store.update_note(id, format!("# Threads\n\nedit {i}")).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(store.get_note(meta.id).unwrap().unwrap().body.contains("edit"));
    }

    struct NullEvents;
    impl PeerEvents for NullEvents {
        fn on_connected(&self, _peer_id: String) {}
        fn on_disconnected(&self, _peer_id: String) {}
    }

    #[test]
    fn two_stores_sync_over_a_real_iroh_mesh() {
        let (_dir_a, a) = open_temp();
        let (_dir_b, b) = open_temp();

        a.create_note("# Mesh\n\nvia ffi sync".into(), "did:a".into()).unwrap();

        let ticket_a = a.pair_ticket().unwrap();
        let ticket_b = b.pair_ticket().unwrap();
        a.add_known_peer(ticket_b).unwrap();
        b.add_known_peer(ticket_a).unwrap();

        a.start_sync(50, Arc::new(NullEvents)).unwrap();
        b.start_sync(50, Arc::new(NullEvents)).unwrap();

        // Generous: this dials by bare EndpointId with no prior direct-address
        // hint, so first contact depends on iroh's discovery (DNS/Pkarr) and
        // relay resolution, which can take tens of seconds depending on
        // network conditions — a real (if occasionally slow), not simulated,
        // property of dial-by-id.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
        loop {
            if b.list_notes().unwrap().iter().any(|n| n.title == "Mesh") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "note never synced over the FFI mesh");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        assert_eq!(a.connected_peers().len(), 1);
        assert_eq!(b.connected_peers().len(), 1);

        a.stop_sync();
        b.stop_sync();
    }
}
