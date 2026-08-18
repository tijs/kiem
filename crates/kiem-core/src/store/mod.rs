//! SQLite-backed note store.
//!
//! Each note row holds the full Automerge document as a BLOB (the source of
//! truth, CRDT history included) next to metadata columns denormalized from
//! [`NoteMetadata`] so listing and filtering never hydrate documents. Columns
//! are recomputed from the document on every write; they are an index, never
//! an authority.
//!
//! The `doc` BLOB is the *same* document across the note's life — updates
//! load it, mutate, and re-save — so CRDT history survives and future sync
//! (U5) can merge concurrent edits. `has_todos` is an extra denormalized
//! column (not part of the Automerge metadata) backing the Todo smart filter.
//!
//! Tag filtering matches exactly (`work` does not match `work/meetings`);
//! nested-tag grouping is a UI concern.
//!
//! `NoteStore` is one type across five files — this one holds the schema,
//! opening, and note CRUD; [`queries`] every read; [`write`] the machinery
//! every mutation funnels through; [`purge`] permanent erasure; [`todos`]
//! checkbox edits. Child modules, so they reach the private plumbing without
//! any of it becoming public API.

use std::collections::HashMap;
use std::path::Path;

use automerge::{AutoCommit, ObjId, ReadDoc, ROOT};
use autosurgeon::{reconcile, Hydrate, Reconcile};
use rusqlite::{params, Connection};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::content;
use crate::note::{NoteDoc, NoteMetadata};
use crate::search::{SearchError, SearchIndex};

/// The purge set as a CRDT: purged note id → true. Map entries only ever get
/// added, so concurrent purges on different devices merge to the union.
///
/// `missing`: a brand-new document has no `purged` key at all, and
/// autosurgeon treats an absent key as an error rather than an empty map
/// (the same trap as `NoteMetadata::status` — see note.rs).
#[derive(Debug, Default, Reconcile, Hydrate)]
struct TombstoneDoc {
    #[autosurgeon(missing = "empty_purged")]
    purged: HashMap<String, bool>,
}

fn empty_purged() -> HashMap<String, bool> {
    HashMap::new()
}

mod purge;
mod queries;
mod todos;
mod write;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("note {0} not found")]
    NotFound(String),
    #[error("note {id} changed since you read it (expected version {expected}, found {found})")]
    VersionMismatch {
        id: String,
        expected: String,
        found: String,
    },
    #[error(
        "editing note {id} would remove its only tag(s) ({tags:?}) — it would drop out of \
         every tag/project filter (including `kiem notes`/`kiem todos` for its project). \
         Keep at least one `#tag` in the replacement text, or narrow the edited line range."
    )]
    TagsWouldBeLost { id: String, tags: Vec<String> },
    #[error("note {0} already exists")]
    DuplicateId(String),
    /// The stored BLOB failed to load or hydrate, or a document failed to
    /// reconcile. Recovery (rebuild from a peer) is a later concern.
    #[error("document error for note {id}: {message}")]
    Document { id: String, message: String },
    #[error(transparent)]
    Search(#[from] SearchError),
    #[error("search is not enabled for this store (open it with open_dir)")]
    SearchDisabled,
    #[error("data directory error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct NoteStore {
    conn: Connection,
    search: Option<SearchIndex>,
}

/// Sidebar smart-filter match counts (see [`NoteStore::filter_counts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterCounts {
    pub todo: u64,
    pub today: u64,
    pub untagged: u64,
    pub pinned: u64,
    pub trash: u64,
}

/// One unchecked todo item belonging to a project's notes. The `(note_id, index)`
/// pair is its address — `index` is the item's position among checkboxes within
/// that note (see [`content::extract_todo_items`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ProjectTodo {
    pub note_id: String,
    pub index: usize,
    pub text: String,
}

/// A hydrated note together with the content-addressed Automerge heads it was
/// read from. Pass [`VersionedNote::version`] to
/// [`NoteStore::update_note_if_version`] to prevent a stale client from
/// replacing a newer document.
#[derive(Debug)]
pub struct VersionedNote {
    pub note: NoteDoc,
    pub version: String,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS notes (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    tags        TEXT NOT NULL, -- JSON array of strings
    author_did  TEXT NOT NULL,
    note_type   TEXT NOT NULL,
    pinned      INTEGER NOT NULL,
    deleted     INTEGER NOT NULL,
    has_todos   INTEGER NOT NULL,
    created_at  TEXT NOT NULL,
    modified_at TEXT NOT NULL,
    status      TEXT,
    doc         BLOB NOT NULL
);
-- Tombstones for permanently erased notes (Empty Trash / Delete Project).
-- The soft `deleted` flag syncs as part of the note's CRDT; a purge removes
-- the row entirely, so without a tombstone the next sync exchange with any
-- peer still holding the document would just resurrect it through `put_doc`.
CREATE TABLE IF NOT EXISTS purged (
    id TEXT PRIMARY KEY
);
-- The purge set as a CRDT document (an Automerge map of purged id → true),
-- synced between peers under a well-known doc id so purges propagate: a peer
-- that receives it erases those notes too. Single row; `purged` above is the
-- fast lookup mirror of this document plus anything applied from peers.
CREATE TABLE IF NOT EXISTS tombstone_doc (
    id  INTEGER PRIMARY KEY CHECK (id = 1),
    doc BLOB NOT NULL
);
";

impl NoteStore {
    /// The search index is a derived, rebuildable structure (see the `search`
    /// module doc), never the source of truth — so a write failing here (e.g.
    /// transient cross-process lock contention with a concurrent CLI command
    /// or another peer's sync ticker) must never fail the note mutation that
    /// already committed to SQLite. Log and move on, same as the sync
    /// ticker's own best-effort `flush_search_index` handling.
    fn log_index_failure(note_id: &str, err: SearchError) {
        eprintln!(
            "kiem: search index update failed for note {note_id}, will retry on next write: {err}"
        );
    }

    /// Open (or create) the full data directory: `kiem.db` plus the `search/`
    /// index. This is what CLI/app surfaces use.
    pub fn open_dir(data_dir: &Path) -> Result<Self, StoreError> {
        ensure_private_data_dir(data_dir)?;
        crate::data_version::check_and_backup(data_dir)?;
        let mut store = Self::open(&data_dir.join("kiem.db"))?;
        store.search = Some(SearchIndex::open_in_dir(&data_dir.join("search"))?);
        Ok(store)
    }

    /// Open (or create) a bare store at a database path — no search index.
    /// WAL mode for concurrent readers.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        // Several processes share one database (sync daemon + one-shot CLI
        // commands): wait out writer locks instead of failing with BUSY.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // The rollback→WAL switch needs exclusive access and reports BUSY
        // without consulting the busy handler, so two processes opening a
        // fresh store race here. WAL is persistent in the file: whoever wins
        // sets it once; losers succeed on a later attempt (then it's a no-op).
        let mut attempts = 0;
        loop {
            match conn.pragma_update(None, "journal_mode", "WAL") {
                Ok(()) => break,
                Err(err) if is_busy(&err) && attempts < 50 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(err) => return Err(err.into()),
            }
        }
        Self::init(conn)
    }

    /// In-memory store for tests (no search index).
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    /// In-memory store with an in-RAM search index, for tests.
    pub fn open_in_memory_with_search() -> Result<Self, StoreError> {
        let mut store = Self::init(Connection::open_in_memory()?)?;
        store.search = Some(SearchIndex::in_memory()?);
        Ok(store)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(SCHEMA)?;
        ensure_status_column(&conn)?;
        Ok(NoteStore { conn, search: None })
    }

    /// Create a note from body text; id and timestamps are generated.
    pub fn create_note(
        &mut self,
        body: &str,
        author_did: &str,
    ) -> Result<NoteMetadata, StoreError> {
        let note = NoteDoc::new(body, author_did);
        self.insert_note(&note)
    }

    /// Create a note with an explicit `note_type` (e.g. `plan`, `review`,
    /// `solution`) so project docs can be grouped by kind. An empty type falls
    /// back to the default; the type is otherwise a free-form label.
    pub fn create_note_with_type(
        &mut self,
        body: &str,
        author_did: &str,
        note_type: &str,
    ) -> Result<NoteMetadata, StoreError> {
        let mut note = NoteDoc::new(body, author_did);
        if !note_type.trim().is_empty() {
            note.metadata.note_type = note_type.trim().to_owned();
        }
        self.insert_note(&note)
    }

    /// Reclassify an existing note's `note_type` (an empty value resets it to the
    /// default). Bumps `modified_at`; leaves body/title/tags untouched.
    pub fn set_note_type(&mut self, id: &str, note_type: &str) -> Result<NoteMetadata, StoreError> {
        let trimmed = note_type.trim();
        let new_type = if trimmed.is_empty() {
            crate::note::DEFAULT_NOTE_TYPE.to_owned()
        } else {
            trimmed.to_owned()
        };
        self.mutate(id, move |note| {
            note.metadata.note_type = new_type;
            note.metadata.modified_at = now_rfc3339();
        })
    }

    /// Insert a pre-built note (deterministic seam; sync will also use it).
    pub fn insert_note(&mut self, note: &NoteDoc) -> Result<NoteMetadata, StoreError> {
        let mut doc = AutoCommit::new();
        reconcile(&mut doc, note).map_err(|e| StoreError::Document {
            id: note.metadata.id.clone(),
            message: e.to_string(),
        })?;
        let m = &note.metadata;
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO notes
             (id, title, tags, author_did, note_type, pinned, deleted, has_todos, created_at, modified_at, status, doc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                m.id,
                m.title,
                tags_json(&m.tags),
                m.author_did,
                m.note_type,
                m.pinned,
                m.deleted,
                content::has_unchecked_todos(note.body.as_str()),
                m.created_at,
                m.modified_at,
                m.status,
                doc.save(),
            ],
        )?;
        if inserted == 0 {
            return Err(StoreError::DuplicateId(m.id.clone()));
        }
        if let Some(index) = &mut self.search {
            if !m.deleted {
                if let Err(e) = index.index_note(m, note.body.as_str()) {
                    Self::log_index_failure(&m.id, e);
                }
            }
        }
        Ok(m.clone())
    }

    /// Run many mutations as one unit: a single SQLite transaction, with
    /// search indexing deferred to one rebuild at the end — the per-note
    /// tantivy commit and journal fsync made a 400-note import take over a
    /// minute. The search index is parked (`Option::take`) for the duration,
    /// so the per-note indexing branches skip naturally; on error (or a panic
    /// in `work`) the transaction rolls back, the index untouched and restored.
    ///
    /// Not re-entrant: a nested `bulk` fails at its inner BEGIN.
    pub fn bulk<T, E: From<StoreError>>(
        &mut self,
        work: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| E::from(StoreError::from(e)))?;
        let parked_search = self.search.take();
        // Catch a panic in `work` so the parked index and open transaction
        // don't outlive it — otherwise search is gone for good and every later
        // write silently joins a zombie transaction. Safe to assert unwind
        // safety: both invariants are restored right here before re-raising.
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(self)));
        self.search = parked_search;
        let result = match caught {
            Ok(result) => result,
            Err(payload) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                std::panic::resume_unwind(payload);
            }
        };
        match result {
            Ok(value) => {
                if let Err(e) = self.conn.execute_batch("COMMIT") {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(E::from(StoreError::from(e)));
                }
                if self.search.is_some() {
                    // A rebuild failure here surfaces as Err even though the
                    // data committed fine — confusing but data-safe (a retry
                    // re-reports the notes as duplicates).
                    self.rebuild_search_index().map_err(E::from)?;
                }
                Ok(value)
            }
            Err(err) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(err)
            }
        }
    }

    /// Replace the note body. Title/tags/todos/modified_at are re-derived and
    /// the existing Automerge document is spliced (history preserved). The
    /// write is still compare-and-swap guarded against another process writing
    /// after this method read the document; use
    /// [`Self::update_note_if_version`] when the caller already has a read
    /// version and needs to reject a stale edit before deriving it.
    pub fn update_note(&mut self, id: &str, body: &str) -> Result<NoteMetadata, StoreError> {
        self.write_body(id, body)
    }

    /// Replace the body only if `expected_version` is still the document heads
    /// read by the caller. This is the cross-process optimistic-concurrency
    /// API used by the GUI editor; a conflict leaves the newer SQLite document
    /// untouched and returns [`StoreError::VersionMismatch`].
    pub fn update_note_if_version(
        &mut self,
        id: &str,
        body: &str,
        expected_version: &str,
    ) -> Result<NoteMetadata, StoreError> {
        self.write_body_if_version(id, body, expected_version)
    }

    /// Add a body-derived hashtag. Idempotent when the note already carries it.
    pub fn add_tag(&mut self, id: &str, tag: &str) -> Result<NoteMetadata, StoreError> {
        let note = self
            .get_note(id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        let body = crate::project::ensure_tag(note.body.as_str(), tag);
        if body == note.body.as_str() {
            Ok(note.metadata)
        } else {
            self.write_body(id, &body)
        }
    }

    /// Remove a body-derived hashtag. Unlike generic body replacement, this
    /// explicit operation may intentionally leave the note untagged.
    pub fn remove_tag(&mut self, id: &str, tag: &str) -> Result<NoteMetadata, StoreError> {
        let note = self
            .get_note(id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        let body = content::remove_tag(note.body.as_str(), tag);
        if body == note.body.as_str() {
            Ok(note.metadata)
        } else {
            self.write_body_inner(id, &body, true, None)
        }
    }

    /// Replace the 1-based inclusive line range `start..=end` of a note's body
    /// with `replacement`, applied as a scalar-correct splice. When
    /// `expect_version` is `Some`, the edit is rejected with
    /// [`StoreError::VersionMismatch`] unless it matches the note's current
    /// version — optimistic concurrency for agents editing shared state.
    pub fn edit_lines(
        &mut self,
        id: &str,
        expect_version: Option<&str>,
        start: usize,
        end: usize,
        replacement: &str,
    ) -> Result<NoteMetadata, StoreError> {
        let (mut doc, _) = self
            .load_doc_with_bytes(id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        if let Some(expected) = expect_version {
            let found = doc_version(&mut doc);
            if found != expected {
                return Err(StoreError::VersionMismatch {
                    id: id.to_owned(),
                    expected: expected.to_owned(),
                    found,
                });
            }
        }
        let (_, old) = body_obj(&doc, id)?;
        let new_body = content::replace_lines(&old, start, end, replacement)
            .map_err(|e| document_err(id, e))?;
        match expect_version {
            Some(version) => self.write_body_if_version(id, &new_body, version),
            None => self.write_body(id, &new_body),
        }
    }

    pub fn set_pinned(&mut self, id: &str, pinned: bool) -> Result<NoteMetadata, StoreError> {
        self.mutate(id, |note| note.set_pinned(pinned))
    }

    /// Soft delete: the note stays retrievable by id and listed under Trash.
    pub fn delete_note(&mut self, id: &str) -> Result<NoteMetadata, StoreError> {
        self.mutate(id, |note| note.set_deleted(true))
    }

    pub fn restore_note(&mut self, id: &str) -> Result<NoteMetadata, StoreError> {
        self.mutate(id, |note| note.set_deleted(false))
    }
}

/// Creates the on-disk data directory and, on Unix, repairs it to owner-only
/// access before any database, identity, or pairing metadata is opened.
/// Non-Unix platforms deliberately retain their native ACL semantics.
fn ensure_private_data_dir(data_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Add the `status` column to a pre-existing `notes` table that predates it.
/// `CREATE TABLE IF NOT EXISTS` is a no-op against an already-existing table
/// with a different column set, so a fresh install gets `status` straight from
/// [`SCHEMA`] but an existing on-disk database needs this guarded migration —
/// the first one this store has ever needed (see the data dir's own
/// whole-directory backup in `data_version.rs`, which is deliberately blunt
/// rather than schema-aware).
fn ensure_status_column(conn: &Connection) -> rusqlite::Result<()> {
    let has_status: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('notes') WHERE name = 'status'")?
        .exists([])?;
    if !has_status {
        conn.execute("ALTER TABLE notes ADD COLUMN status TEXT", [])?;
    }
    Ok(())
}

fn tags_json(tags: &[String]) -> String {
    serde_json::to_string(tags).expect("a Vec<String> always serializes to JSON")
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("RFC 3339 formatting of a valid UTC time cannot fail")
}

/// A note's content-addressed version: its Automerge document heads as hex.
/// Takes `&mut` because `AutoCommit::get_heads` flushes the pending transaction.
fn doc_version(doc: &mut AutoCommit) -> String {
    doc.get_heads().iter().map(|h| h.to_string()).collect()
}

/// The note's body Text object and its current value. The body is a Text object
/// at the document root (`NoteDoc.body`); its absence means a corrupt document.
fn body_obj(doc: &AutoCommit, id: &str) -> Result<(ObjId, String), StoreError> {
    let (_, obj) = doc
        .get(ROOT, "body")
        .map_err(|e| document_err(id, e))?
        .ok_or_else(|| document_err(id, "note document has no body text object"))?;
    let text = doc.text(&obj).map_err(|e| document_err(id, e))?;
    Ok((obj, text))
}

fn is_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

fn document_err(id: &str, e: impl std::fmt::Display) -> StoreError {
    StoreError::Document {
        id: id.to_owned(),
        message: e.to_string(),
    }
}
