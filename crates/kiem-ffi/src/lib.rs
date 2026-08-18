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

use kiem_core::store::NoteStore;
use kiem_core::sync::SyncEngine;
use kiem_sync::{SharedState, SyncState};

uniffi::setup_scaffolding!();

mod records;
mod sync;
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
    /// Called whenever a sync message is sent to or received from a peer, so
    /// the UI can show a transient "syncing" indicator.
    fn on_sync_activity(&self, peer_id: String);
    /// An unknown peer dialed in during an open pairing window — return true to
    /// trust it. Called on a blocking thread, so the Swift side may wait on a
    /// user prompt (e.g. a semaphore released by an Allow/Deny sheet).
    fn approve_pairing(&self, peer_id: String) -> bool;
}

pub(crate) struct EventsAdapter(Arc<dyn PeerEvents>);

impl kiem_sync::MeshEvents for EventsAdapter {
    fn on_connected(&self, peer: kiem_sync::EndpointId) {
        self.0.on_connected(peer.to_string());
    }
    fn on_disconnected(&self, peer: kiem_sync::EndpointId) {
        self.0.on_disconnected(peer.to_string());
    }
    fn on_sync_activity(&self, peer: kiem_sync::EndpointId) {
        self.0.on_sync_activity(peer.to_string());
    }
    fn on_error(&self, context: &str, error: &str) {
        eprintln!("kiem sync: {context}: {error}");
    }
    fn approve_pairing(&self, peer: kiem_sync::EndpointId) -> bool {
        self.0.approve_pairing(peer.to_string())
    }
}

pub(crate) struct SyncHandle {
    pub(crate) runtime: tokio::runtime::Runtime,
    pub(crate) mesh: Arc<kiem_sync::Mesh>,
}

#[derive(uniffi::Object)]
pub struct KiemStore {
    pub(crate) data_dir: PathBuf,
    pub(crate) state: SharedState,
    pub(crate) sync: Mutex<Option<SyncHandle>>,
}

impl KiemStore {
    /// Run `f` with the store+engine lock held for the whole operation.
    fn with<T>(
        &self,
        f: impl FnOnce(&mut NoteStore, &mut SyncEngine) -> Result<T, KiemError>,
    ) -> Result<T, KiemError> {
        let mut guard = self.state.lock();
        let SyncState { store, engine } = &mut *guard;
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
            state: kiem_sync::shared_state(store),
            sync: Mutex::new(None),
        })
    }

    pub fn create_note(&self, body: String, author_did: String) -> Result<NoteMetadata, KiemError> {
        self.with(|store, _| Ok(store.create_note(&body, &author_did)?.into()))
    }

    pub fn get_note(&self, id: String) -> Result<Option<Note>, KiemError> {
        self.with(|store, _| {
            Ok(store.get_note_with_version(&id)?.map(|versioned| Note {
                body: versioned.note.body.as_str().to_owned(),
                metadata: versioned.note.metadata.into(),
                version: versioned.version,
            }))
        })
    }

    /// Holds the lock for the full hydrate→edit→reconcile cycle so an
    /// incoming sync message cannot interleave (autosurgeon StaleHeads).
    pub fn update_note(&self, id: String, body: String) -> Result<NoteMetadata, KiemError> {
        self.with(|store, _| Ok(store.update_note(&id, &body)?.into()))
    }

    /// Replace an editor buffer only if it still descends from the version
    /// returned by `get_note`. The typed `KiemError::Conflict` lets Swift
    /// reload the external document rather than blindly overwriting it.
    pub fn update_note_if_version(
        &self,
        id: String,
        body: String,
        expected_version: String,
    ) -> Result<Note, KiemError> {
        self.with(|store, _| {
            store.update_note_if_version(&id, &body, &expected_version)?;
            let versioned = store
                .get_note_with_version(&id)?
                .ok_or_else(|| kiem_core::store::StoreError::NotFound(id.clone()))?;
            Ok(Note {
                body: versioned.note.body.as_str().to_owned(),
                metadata: versioned.note.metadata.into(),
                version: versioned.version,
            })
        })
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
}

pub(crate) fn sync_err(err: impl std::fmt::Display) -> KiemError {
    KiemError::Sync {
        message: err.to_string(),
    }
}

fn into_meta(metas: Vec<kiem_core::note::NoteMetadata>) -> Vec<NoteMetadata> {
    metas.into_iter().map(Into::into).collect()
}
