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

use std::path::Path;

use automerge::{AutoCommit, ObjId, ReadDoc, ROOT};
use autosurgeon::{hydrate, reconcile};
use rusqlite::{params, Connection, OptionalExtension, Row};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::content;
use crate::note::{NoteDoc, NoteMetadata};
use crate::search::{SearchError, SearchIndex, SearchResult};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("note {0} not found")]
    NotFound(String),
    #[error("note {id} changed since you read it (expected version {expected}, found {found})")]
    VersionMismatch { id: String, expected: String, found: String },
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
    doc         BLOB NOT NULL
);
";

const META_COLUMNS: &str =
    "id, title, tags, author_did, note_type, pinned, deleted, created_at, modified_at";

impl NoteStore {
    /// Open (or create) the full data directory: `kiem.db` plus the `search/`
    /// index. This is what CLI/app surfaces use.
    pub fn open_dir(data_dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(data_dir)?;
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
        Ok(NoteStore { conn, search: None })
    }

    /// Create a note from body text; id and timestamps are generated.
    pub fn create_note(&mut self, body: &str, author_did: &str) -> Result<NoteMetadata, StoreError> {
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

    /// Notes carrying `tag`, filtered to a single `note_type` (most recent
    /// first). Backs `kiem notes --type` and the app's per-project grouping.
    pub fn list_by_tag_and_type(
        &self,
        tag: &str,
        note_type: &str,
    ) -> Result<Vec<NoteMetadata>, StoreError> {
        Ok(self
            .list_by_tag(tag)?
            .into_iter()
            .filter(|m| m.note_type == note_type)
            .collect())
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
             (id, title, tags, author_did, note_type, pinned, deleted, has_todos, created_at, modified_at, doc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                doc.save(),
            ],
        )?;
        if inserted == 0 {
            return Err(StoreError::DuplicateId(m.id.clone()));
        }
        if let Some(index) = &mut self.search {
            if !m.deleted {
                index.index_note(m, note.body.as_str())?;
            }
        }
        Ok(m.clone())
    }

    /// Full-text search over title, body, and tags of live (non-deleted)
    /// notes. Requires a store opened with [`open_dir`](Self::open_dir).
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, StoreError> {
        let index = self.search.as_ref().ok_or(StoreError::SearchDisabled)?;
        Ok(index.search(query, limit)?)
    }

    /// Recovery path: drop the search index contents and re-feed every live
    /// note from SQLite (the index is derived, never authoritative).
    pub fn rebuild_search_index(&mut self) -> Result<(), StoreError> {
        if self.search.is_none() {
            return Err(StoreError::SearchDisabled);
        }
        let metas = self.list_notes()?;
        let mut entries = Vec::with_capacity(metas.len());
        for meta in metas {
            let note = self
                .get_note(&meta.id)?
                .ok_or_else(|| StoreError::NotFound(meta.id.clone()))?;
            entries.push((meta, note.body.as_str().to_owned()));
        }
        let index = self.search.as_mut().expect("checked above");
        index.rebuild(entries.iter().map(|(m, b)| (m, b.as_str())))?;
        Ok(())
    }

    /// Raw Automerge document bytes (the persisted source of truth). The sync
    /// engine exchanges these; UIs should prefer [`get_note`](Self::get_note).
    pub fn get_doc_bytes(&self, id: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT doc FROM notes WHERE id = ?1", params![id], |r| r.get(0))
            .optional()?)
    }

    /// Load and hydrate the full note document. `None` if the id is unknown.
    pub fn get_note(&self, id: &str) -> Result<Option<NoteDoc>, StoreError> {
        match self.load_doc(id)? {
            None => Ok(None),
            Some(doc) => hydrate(&doc)
                .map(Some)
                .map_err(|e| document_err(id, e)),
        }
    }

    /// Replace the note body. Title/tags/todos/modified_at are re-derived and
    /// the existing Automerge document is spliced (history preserved).
    pub fn update_note(&mut self, id: &str, body: &str) -> Result<NoteMetadata, StoreError> {
        self.write_body(id, body)
    }

    /// A short, content-addressed version token for a note (its Automerge
    /// document heads, hex). Read it alongside a note, pass it back as
    /// `expect_version` to [`edit_lines`](Self::edit_lines) to reject an edit if
    /// the note changed underneath you (the todo-index / concurrent-edit race).
    pub fn note_version(&self, id: &str) -> Result<String, StoreError> {
        let mut doc = self.load_doc(id)?.ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        Ok(doc_version(&mut doc))
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
        let mut doc = self.load_doc(id)?.ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
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
        self.write_body(id, &new_body)
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

    /// All non-deleted notes, most recently modified first.
    pub fn list_notes(&self) -> Result<Vec<NoteMetadata>, StoreError> {
        self.query_meta("deleted = 0", params![])
    }

    /// Notes carrying exactly `tag` (no nested-prefix matching).
    pub fn list_by_tag(&self, tag: &str) -> Result<Vec<NoteMetadata>, StoreError> {
        self.query_meta(
            "deleted = 0 AND EXISTS (SELECT 1 FROM json_each(notes.tags) WHERE json_each.value = ?1)",
            params![tag],
        )
    }

    /// Notes with at least one unchecked `- [ ]` item.
    pub fn list_todos(&self) -> Result<Vec<NoteMetadata>, StoreError> {
        self.query_meta("deleted = 0 AND has_todos = 1", params![])
    }

    /// Notes last modified on the given UTC calendar date (`YYYY-MM-DD`).
    pub fn list_modified_on(&self, date_utc: &str) -> Result<Vec<NoteMetadata>, StoreError> {
        self.query_meta(
            "deleted = 0 AND substr(modified_at, 1, 10) = ?1",
            params![date_utc],
        )
    }

    /// Notes last modified today (UTC).
    pub fn list_today(&self) -> Result<Vec<NoteMetadata>, StoreError> {
        let now = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .expect("RFC 3339 formatting of a valid UTC time cannot fail");
        self.list_modified_on(&now[..10])
    }

    pub fn list_untagged(&self) -> Result<Vec<NoteMetadata>, StoreError> {
        self.query_meta("deleted = 0 AND tags = '[]'", params![])
    }

    pub fn list_pinned(&self) -> Result<Vec<NoteMetadata>, StoreError> {
        self.query_meta("deleted = 0 AND pinned = 1", params![])
    }

    pub fn list_deleted(&self) -> Result<Vec<NoteMetadata>, StoreError> {
        self.query_meta("deleted = 1", params![])
    }

    /// Every note id including trashed notes — sync replicates everything.
    pub fn list_all_ids(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT id FROM notes ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Persist a document that changed outside the normal edit path (sync
    /// receive). Inserts or fully replaces the row from the document's own
    /// hydrated state and keeps the search index in step.
    pub fn put_doc(&mut self, doc: &mut AutoCommit) -> Result<NoteMetadata, StoreError> {
        let note: NoteDoc = hydrate(doc).map_err(|e| document_err("(sync)", e))?;
        let m = &note.metadata;
        self.conn.execute(
            "INSERT OR REPLACE INTO notes
             (id, title, tags, author_did, note_type, pinned, deleted, has_todos, created_at, modified_at, doc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                doc.save(),
            ],
        )?;
        if let Some(index) = &mut self.search {
            if m.deleted {
                index.remove_note(&m.id)?;
            } else {
                index.index_note(m, note.body.as_str())?;
            }
        }
        Ok(note.metadata)
    }

    /// Every tag on live notes with its usage count, alphabetical.
    pub fn list_tags(&self) -> Result<Vec<(String, usize)>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT tags FROM notes WHERE deleted = 0")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut counts = std::collections::BTreeMap::new();
        for tags_text in rows {
            for tag in serde_json::from_str::<Vec<String>>(&tags_text?).unwrap_or_default() {
                *counts.entry(tag).or_insert(0usize) += 1;
            }
        }
        Ok(counts.into_iter().collect())
    }

    /// All unchecked todo items across live notes carrying `tag`, in note-list
    /// order (most recently modified first) then document order. Each item's
    /// `(note_id, index)` addresses it for [`set_todo_checked`](Self::set_todo_checked).
    pub fn list_todo_items_for_tag(&self, tag: &str) -> Result<Vec<ProjectTodo>, StoreError> {
        let mut out = Vec::new();
        for meta in self.list_by_tag(tag)? {
            let note = self
                .get_note(&meta.id)?
                .ok_or_else(|| StoreError::NotFound(meta.id.clone()))?;
            for item in content::extract_todo_items(note.body.as_str()) {
                if !item.checked {
                    out.push(ProjectTodo { note_id: meta.id.clone(), index: item.index, text: item.text });
                }
            }
        }
        Ok(out)
    }

    /// Toggle the checkbox at `index` within note `id` and persist. Goes through
    /// the normal body-update path, so title/tags/`modified_at` re-derive and the
    /// change splices into the existing Automerge document (sync-safe).
    pub fn set_todo_checked(
        &mut self,
        id: &str,
        index: usize,
        checked: bool,
    ) -> Result<NoteMetadata, StoreError> {
        let note = self
            .get_note(id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        let new_body = content::set_todo_checked(note.body.as_str(), index, checked)
            .map_err(|e| document_err(id, e))?;
        self.update_note(id, &new_body)
    }

    /// Append a new unchecked todo to note `id` and persist. Goes through the
    /// normal body-update path (title/tags/`modified_at` re-derive, splices into
    /// the existing Automerge document), so it is sync-safe like an edit.
    pub fn add_todo(&mut self, id: &str, text: &str) -> Result<NoteMetadata, StoreError> {
        let note = self
            .get_note(id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        let new_body = content::append_todo(note.body.as_str(), text);
        self.update_note(id, &new_body)
    }

    // -- internals --

    /// Hydrate → mutate → reconcile into the *same* document, then persist.
    /// The store owns the document for the whole cycle, the single-connection
    /// equivalent of the U6 rule that edits and sync-receives serialize per
    /// document (autosurgeon StaleHeads).
    fn mutate(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut NoteDoc),
    ) -> Result<NoteMetadata, StoreError> {
        let mut doc = self
            .load_doc(id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        let mut note: NoteDoc = hydrate(&doc).map_err(|e| document_err(id, e))?;
        change(&mut note);
        reconcile(&mut doc, &note).map_err(|e| document_err(id, e))?;
        self.persist(id, &mut doc, &note)?;
        Ok(note.metadata)
    }

    /// Replace a note's body via a **scalar-indexed** Automerge text splice, then
    /// re-derive metadata. This bypasses autosurgeon's `Text::update`, whose
    /// byte-offset splice corrupts any body containing a multi-byte character
    /// (see [`content::body_splice`]). Metadata reconciles normally — the body
    /// object carries no autosurgeon edits, so `reconcile` leaves it untouched.
    fn write_body(&mut self, id: &str, new_body: &str) -> Result<NoteMetadata, StoreError> {
        use automerge::transaction::Transactable;
        let mut doc = self
            .load_doc(id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        let (obj, old) = body_obj(&doc, id)?;
        if let Some(s) = content::body_splice(&old, new_body) {
            doc.splice_text(&obj, s.pos, s.del as isize, &s.insert)
                .map_err(|e| document_err(id, e))?;
        }
        let mut note: NoteDoc = hydrate(&doc).map_err(|e| document_err(id, e))?;
        note.metadata.title = content::derive_title(new_body);
        note.metadata.tags = content::extract_tags(new_body);
        note.metadata.modified_at = now_rfc3339();
        reconcile(&mut doc, &note).map_err(|e| document_err(id, e))?;
        self.persist(id, &mut doc, &note)?;
        Ok(note.metadata)
    }

    /// Write the note's denormalized columns + saved document to SQLite and
    /// refresh the search index. Shared by [`mutate`](Self::mutate) and
    /// [`write_body`](Self::write_body).
    fn persist(&mut self, _id: &str, doc: &mut AutoCommit, note: &NoteDoc) -> Result<(), StoreError> {
        let m = &note.metadata;
        self.conn.execute(
            "UPDATE notes SET title = ?2, tags = ?3, pinned = ?4, deleted = ?5,
             has_todos = ?6, modified_at = ?7, doc = ?8 WHERE id = ?1",
            params![
                m.id,
                m.title,
                tags_json(&m.tags),
                m.pinned,
                m.deleted,
                content::has_unchecked_todos(note.body.as_str()),
                m.modified_at,
                doc.save(),
            ],
        )?;
        if let Some(index) = &mut self.search {
            if m.deleted {
                index.remove_note(&m.id)?;
            } else {
                index.index_note(m, note.body.as_str())?;
            }
        }
        Ok(())
    }

    fn load_doc(&self, id: &str) -> Result<Option<AutoCommit>, StoreError> {
        match self.get_doc_bytes(id)? {
            None => Ok(None),
            Some(b) => AutoCommit::load(&b).map(Some).map_err(|e| document_err(id, e)),
        }
    }

    fn query_meta(
        &self,
        predicate: &str,
        args: impl rusqlite::Params,
    ) -> Result<Vec<NoteMetadata>, StoreError> {
        let sql = format!(
            "SELECT {META_COLUMNS} FROM notes WHERE {predicate} ORDER BY modified_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(args, row_to_meta)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

fn row_to_meta(row: &Row<'_>) -> rusqlite::Result<NoteMetadata> {
    let tags_text: String = row.get("tags")?;
    Ok(NoteMetadata {
        id: row.get("id")?,
        title: row.get("title")?,
        tags: serde_json::from_str(&tags_text).unwrap_or_default(),
        author_did: row.get("author_did")?,
        note_type: row.get("note_type")?,
        pinned: row.get("pinned")?,
        deleted: row.get("deleted")?,
        created_at: row.get("created_at")?,
        modified_at: row.get("modified_at")?,
    })
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
