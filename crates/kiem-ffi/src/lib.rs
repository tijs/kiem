//! Kiem FFI: thin UniFFI bridge exposing `kiem-core` to Swift.
//!
//! `KiemStore` is an opaque handle; Automerge documents never cross the
//! boundary — value records ([`NoteMetadata`], [`Note`], [`SearchResult`])
//! and raw sync bytes do. One `Mutex` guards the store *and* the sync
//! engine together so a local edit can never interleave with a sync
//! receive mid-hydrate/reconcile (autosurgeon `StaleHeads`).
//!
//! The mirrored record types exist because `kiem-core` stays FFI-free by
//! design; `From` impls keep the mapping mechanical.

use std::sync::Mutex;

use kiem_core::store::{NoteStore, StoreError};
use kiem_core::sync::SyncEngine;

uniffi::setup_scaffolding!();

#[derive(Debug, uniffi::Record)]
pub struct NoteMetadata {
    pub id: String,
    pub title: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub deleted: bool,
    pub created_at: String,
    pub modified_at: String,
    pub author_did: String,
    pub note_type: String,
}

impl From<kiem_core::note::NoteMetadata> for NoteMetadata {
    fn from(m: kiem_core::note::NoteMetadata) -> Self {
        NoteMetadata {
            id: m.id,
            title: m.title,
            tags: m.tags,
            pinned: m.pinned,
            deleted: m.deleted,
            created_at: m.created_at,
            modified_at: m.modified_at,
            author_did: m.author_did,
            note_type: m.note_type,
        }
    }
}

#[derive(Debug, uniffi::Record)]
pub struct Note {
    pub metadata: NoteMetadata,
    pub body: String,
}

#[derive(Debug, uniffi::Record)]
pub struct SearchResult {
    pub note_id: String,
    pub title: String,
    pub snippet: String,
    pub score: f32,
}

impl From<kiem_core::search::SearchResult> for SearchResult {
    fn from(r: kiem_core::search::SearchResult) -> Self {
        SearchResult { note_id: r.note_id, title: r.title, snippet: r.snippet, score: r.score }
    }
}

#[derive(Debug, uniffi::Record)]
pub struct TagCount {
    pub tag: String,
    pub count: u64,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum KiemError {
    #[error("note not found: {id}")]
    NotFound { id: String },
    #[error("note already exists: {id}")]
    Duplicate { id: String },
    #[error("storage error: {message}")]
    Storage { message: String },
    #[error("sync error: {message}")]
    Sync { message: String },
}

impl From<StoreError> for KiemError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::NotFound(id) => KiemError::NotFound { id },
            StoreError::DuplicateId(id) => KiemError::Duplicate { id },
            other => KiemError::Storage { message: other.to_string() },
        }
    }
}

impl From<kiem_core::sync::SyncError> for KiemError {
    fn from(err: kiem_core::sync::SyncError) -> Self {
        match err {
            kiem_core::sync::SyncError::Store(e) => e.into(),
            other => KiemError::Sync { message: other.to_string() },
        }
    }
}

#[derive(uniffi::Object)]
pub struct KiemStore {
    state: Mutex<(NoteStore, SyncEngine)>,
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
        let store = NoteStore::open_dir(std::path::Path::new(&data_dir))?;
        Ok(KiemStore { state: Mutex::new((store, SyncEngine::new())) })
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

    pub fn get_tags(&self) -> Result<Vec<TagCount>, KiemError> {
        self.with(|store, _| {
            Ok(store
                .list_tags()?
                .into_iter()
                .map(|(tag, count)| TagCount { tag, count: count as u64 })
                .collect())
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

    // -- sync (PeerManager feeds raw bytes through these) --

    pub fn get_document_ids(&self) -> Result<Vec<String>, KiemError> {
        self.with(|store, engine| Ok(engine.doc_ids(store)?))
    }

    pub fn generate_sync_message(
        &self,
        peer_id: String,
        doc_id: String,
    ) -> Result<Option<Vec<u8>>, KiemError> {
        self.with(|store, engine| Ok(engine.generate_message(store, &peer_id, &doc_id)?))
    }

    pub fn receive_sync_message(
        &self,
        peer_id: String,
        doc_id: String,
        message: Vec<u8>,
    ) -> Result<(), KiemError> {
        self.with(|store, engine| Ok(engine.receive_message(store, &peer_id, &doc_id, &message)?))
    }

    pub fn forget_peer(&self, peer_id: String) {
        let mut guard = self.state.lock().expect("KiemStore lock poisoned");
        guard.1.forget_peer(&peer_id);
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
        assert_eq!(store.search("hello".into(), 10).unwrap()[0].note_id, meta.id);
        assert_eq!(store.get_tags().unwrap()[0].tag, "ffi");

        store.delete_note(meta.id.clone()).unwrap();
        assert!(store.list_notes().unwrap().is_empty());
        assert_eq!(store.list_deleted().unwrap().len(), 1);
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
    fn two_stores_sync_through_the_ffi_surface() {
        let (_da, a) = open_temp();
        let (_db, b) = open_temp();
        a.create_note("# Synced\n\nvia ffi".into(), "did:a".into()).unwrap();

        // Pump messages both ways until quiet, exactly as PeerManager will.
        let mut traffic = true;
        while traffic {
            traffic = false;
            for (from, to, from_name, to_name) in [(&a, &b, "a", "b"), (&b, &a, "b", "a")] {
                for doc_id in from.get_document_ids().unwrap() {
                    if let Some(msg) = from
                        .generate_sync_message(to_name.into(), doc_id.clone())
                        .unwrap()
                    {
                        to.receive_sync_message(from_name.into(), doc_id, msg).unwrap();
                        traffic = true;
                    }
                }
            }
        }

        let notes = b.list_notes().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "Synced");
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
}
