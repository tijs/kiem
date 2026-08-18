//! Value records and errors crossing the UniFFI boundary, mirrored from
//! `kiem-core` types (the core stays FFI-free by design; `From` impls keep
//! the mapping mechanical). Split out of `lib.rs` (file-size limit).

use kiem_core::store::StoreError;

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
    /// From a leading `---\nstatus: <value>\n---` frontmatter fence in the
    /// body, if present. `None` for the overwhelming majority of notes.
    pub status: Option<String>,
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
            status: m.status,
        }
    }
}

#[derive(Debug, uniffi::Record)]
pub struct Note {
    pub metadata: NoteMetadata,
    pub body: String,
    /// Content-addressed Automerge heads for this read. Supply it to
    /// `KiemStore.update_note_if_version` to reject a stale whole-body edit.
    pub version: String,
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
        SearchResult {
            note_id: r.note_id,
            title: r.title,
            snippet: r.snippet,
            score: r.score,
        }
    }
}

#[derive(Debug, uniffi::Record)]
pub struct TagCount {
    pub tag: String,
    pub count: u64,
}

/// Sidebar smart-filter match counts, computed in one table scan.
#[derive(Debug, uniffi::Record)]
pub struct FilterCounts {
    pub todo: u64,
    pub today: u64,
    pub untagged: u64,
    pub pinned: u64,
    pub trash: u64,
}

impl From<kiem_core::store::FilterCounts> for FilterCounts {
    fn from(c: kiem_core::store::FilterCounts) -> Self {
        FilterCounts {
            todo: c.todo,
            today: c.today,
            untagged: c.untagged,
            pinned: c.pinned,
            trash: c.trash,
        }
    }
}

#[derive(Debug, uniffi::Record)]
pub struct ProjectTodo {
    pub note_id: String,
    /// Position among the note's checkboxes — its address for `set_todo_checked`.
    pub index: u32,
    pub text: String,
}

impl From<kiem_core::store::ProjectTodo> for ProjectTodo {
    fn from(t: kiem_core::store::ProjectTodo) -> Self {
        ProjectTodo {
            note_id: t.note_id,
            index: t.index as u32,
            text: t.text,
        }
    }
}

/// Counts from a notes export/import: `transferred` = files written (export)
/// or notes created (import); `skipped` = notes without a project (export) or
/// already-present duplicates (import).
#[derive(Debug, uniffi::Record)]
pub struct TransferSummary {
    pub transferred: u32,
    pub skipped: u32,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum KiemError {
    #[error("note not found: {id}")]
    NotFound { id: String },
    #[error("note already exists: {id}")]
    Duplicate { id: String },
    #[error("note {id} changed since it was read (expected version {expected}, found {found})")]
    Conflict {
        id: String,
        expected: String,
        found: String,
    },
    #[error("storage error: {message}")]
    Storage { message: String },
    #[error("sync error: {message}")]
    Sync { message: String },
    // Import/export I/O — the message already names the file and operation.
    #[error("{message}")]
    Transfer { message: String },
}

impl From<StoreError> for KiemError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::NotFound(id) => KiemError::NotFound { id },
            StoreError::DuplicateId(id) => KiemError::Duplicate { id },
            StoreError::VersionMismatch {
                id,
                expected,
                found,
            } => KiemError::Conflict {
                id,
                expected,
                found,
            },
            other => KiemError::Storage {
                message: other.to_string(),
            },
        }
    }
}

impl From<kiem_core::transfer::TransferError> for KiemError {
    fn from(err: kiem_core::transfer::TransferError) -> Self {
        match err {
            kiem_core::transfer::TransferError::Store(e) => e.into(),
            other => KiemError::Transfer {
                message: other.to_string(),
            },
        }
    }
}

impl From<kiem_core::sync::SyncError> for KiemError {
    fn from(err: kiem_core::sync::SyncError) -> Self {
        match err {
            kiem_core::sync::SyncError::Store(e) => e.into(),
            other => KiemError::Sync {
                message: other.to_string(),
            },
        }
    }
}
