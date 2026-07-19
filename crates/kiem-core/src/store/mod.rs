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

use std::collections::HashMap;
use std::path::Path;

use automerge::{AutoCommit, ObjId, ReadDoc, ROOT};
use autosurgeon::{hydrate, reconcile, Hydrate, Reconcile};
use rusqlite::{params, Connection};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::content;
use crate::note::{NoteDoc, NoteMetadata};
use crate::search::{SearchError, SearchIndex};
use crate::sync::TOMBSTONES_DOC_ID;

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

mod queries;

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
    /// Open (or create) the full data directory: `kiem.db` plus the `search/`
    /// index. This is what CLI/app surfaces use.
    pub fn open_dir(data_dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(data_dir)?;
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
                index.index_note(m, note.body.as_str())?;
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
    /// the existing Automerge document is spliced (history preserved).
    pub fn update_note(&mut self, id: &str, body: &str) -> Result<NoteMetadata, StoreError> {
        self.write_body(id, body)
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
            self.write_body_inner(id, &body, true)
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
        let mut doc = self
            .load_doc(id)?
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

    /// Permanently erase every trashed note: the rows (and their Automerge
    /// documents) are deleted and each id is tombstoned in `purged`, so a
    /// later sync receive from a peer that still holds the note cannot
    /// resurrect it. Returns how many notes were erased.
    pub fn purge_deleted(&mut self) -> Result<usize, StoreError> {
        let ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM notes WHERE deleted = 1")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        self.purge_ids(&ids)
    }

    /// Permanently erase a project: every note carrying `tag` — trashed ones
    /// included — is deleted and tombstoned, exactly like
    /// [`purge_deleted`](Self::purge_deleted). Returns how many notes were
    /// erased. A note tagged into several projects is erased with the one
    /// being deleted.
    pub fn purge_tag(&mut self, tag: &str) -> Result<usize, StoreError> {
        let ids: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT id FROM notes
                 WHERE EXISTS (SELECT 1 FROM json_each(notes.tags) WHERE json_each.value = ?1)",
            )?;
            let rows = stmt.query_map(params![tag], |row| row.get(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        self.purge_ids(&ids)
    }

    /// Shared permanent-erase machinery for locally-initiated purges:
    /// tombstone + delete the rows, and record the ids in the tombstone
    /// document so the purge propagates to peers on the next sync.
    fn purge_ids(&mut self, ids: &[String]) -> Result<usize, StoreError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut doc = self
            .tombstone_doc_bytes()?
            .map(|bytes| AutoCommit::load(&bytes).map_err(|e| document_err(TOMBSTONES_DOC_ID, e)))
            .transpose()?
            .unwrap_or_else(AutoCommit::new);
        let mut set: TombstoneDoc =
            hydrate(&doc).map_err(|e| document_err(TOMBSTONES_DOC_ID, e))?;
        for id in ids {
            set.purged.insert(id.clone(), true);
        }
        reconcile(&mut doc, &set).map_err(|e| document_err(TOMBSTONES_DOC_ID, e))?;
        self.purge_rows(ids, &doc.save())
    }

    /// The purge set's CRDT document, for syncing under
    /// [`TOMBSTONES_DOC_ID`](crate::sync::TOMBSTONES_DOC_ID). `None` until the
    /// first purge anywhere in the mesh.
    pub fn tombstone_doc_bytes(&self) -> Result<Option<Vec<u8>>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT doc FROM tombstone_doc WHERE id = 1")?;
        let mut rows = stmt.query([])?;
        Ok(rows.next()?.map(|row| row.get(0)).transpose()?)
    }

    /// Adopt a tombstone document that changed through sync: persist it and
    /// apply every purge it lists locally (idempotent for ids already purged).
    /// Returns how many notes this actually erased here.
    pub fn adopt_tombstone_doc(&mut self, doc: &mut AutoCommit) -> Result<usize, StoreError> {
        let set: TombstoneDoc = hydrate(doc).map_err(|e| document_err(TOMBSTONES_DOC_ID, e))?;
        let ids: Vec<String> = set.purged.into_keys().collect();
        self.purge_rows(&ids, &doc.save())
    }

    /// Tombstone the ids, delete their rows, and persist the tombstone
    /// document — one transaction — then drop any search-index entries.
    /// Returns how many ids had a live row (i.e. were newly erased here).
    fn purge_rows(&mut self, ids: &[String], doc_bytes: &[u8]) -> Result<usize, StoreError> {
        let mut erased = 0;
        let tx = self.conn.transaction()?;
        for id in ids {
            tx.execute("INSERT OR IGNORE INTO purged (id) VALUES (?1)", params![id])?;
            erased += tx.execute("DELETE FROM notes WHERE id = ?1", params![id])?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO tombstone_doc (id, doc) VALUES (1, ?1)",
            params![doc_bytes],
        )?;
        tx.commit()?;
        if let Some(index) = &mut self.search {
            for id in ids {
                index.remove_note(id)?;
            }
        }
        Ok(erased)
    }

    /// Persist a document that changed outside the normal edit path (sync
    /// receive). Inserts or fully replaces the row from the document's own
    /// hydrated state and keeps the search index in step.
    pub fn put_doc(&mut self, doc: &mut AutoCommit) -> Result<NoteMetadata, StoreError> {
        let note: NoteDoc = hydrate(doc).map_err(|e| document_err("(sync)", e))?;
        let m = &note.metadata;
        // A purged id was permanently erased (Empty Trash): a peer that still
        // holds the document must not resurrect it here. Accept-and-drop, so
        // the sync session still converges from its point of view.
        let purged: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM purged WHERE id = ?1)",
            params![m.id],
            |row| row.get(0),
        )?;
        if purged {
            return Ok(note.metadata);
        }
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

    /// Toggle one checkbox at `index` within note `id` and persist.
    pub fn set_todo_checked(
        &mut self,
        id: &str,
        index: usize,
        checked: bool,
    ) -> Result<NoteMetadata, StoreError> {
        self.set_todos_checked(id, &[index], checked)
    }

    /// Toggle several checkbox positions in one sync-safe note update.
    ///
    /// Indices address all checkbox lines, including already checked ones, so
    /// checking one item does not renumber the remaining positions. All indices
    /// are applied to an in-memory body before persistence; an invalid index
    /// leaves the note unchanged.
    pub fn set_todos_checked(
        &mut self,
        id: &str,
        indices: &[usize],
        checked: bool,
    ) -> Result<NoteMetadata, StoreError> {
        let note = self
            .get_note(id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        let mut new_body = note.body.as_str().to_owned();
        for &index in indices {
            new_body = content::set_todo_checked(&new_body, index, checked)
                .map_err(|e| document_err(id, e))?;
        }
        self.update_note(id, &new_body)
    }

    /// Replace the text of the todo at `index` within note `id` and persist.
    /// Same sync-safe body-update path as [`Self::set_todo_checked`].
    pub fn set_todo_text(
        &mut self,
        id: &str,
        index: usize,
        text: &str,
    ) -> Result<NoteMetadata, StoreError> {
        let note = self
            .get_note(id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        let new_body = content::set_todo_text(note.body.as_str(), index, text)
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
        self.write_body_inner(id, new_body, false)
    }

    fn write_body_inner(
        &mut self,
        id: &str,
        new_body: &str,
        allow_untagged: bool,
    ) -> Result<NoteMetadata, StoreError> {
        use automerge::transaction::Transactable;
        let mut doc = self
            .load_doc(id)?
            .ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        let (obj, old) = body_obj(&doc, id)?;
        let (_, old_rest) = content::parse_frontmatter_status(&old);
        let old_tags = content::extract_tags(old_rest);
        let (status, new_rest) = content::parse_frontmatter_status(new_body);
        let new_tags = content::extract_tags(new_rest);
        if !allow_untagged && !old_tags.is_empty() && new_tags.is_empty() {
            return Err(StoreError::TagsWouldBeLost {
                id: id.to_owned(),
                tags: old_tags,
            });
        }
        if let Some(s) = content::body_splice(&old, new_body) {
            doc.splice_text(&obj, s.pos, s.del as isize, &s.insert)
                .map_err(|e| document_err(id, e))?;
        }
        let mut note: NoteDoc = hydrate(&doc).map_err(|e| document_err(id, e))?;
        note.metadata.title = content::derive_title(new_rest);
        note.metadata.tags = new_tags;
        note.metadata.status = status;
        note.metadata.modified_at = now_rfc3339();
        reconcile(&mut doc, &note).map_err(|e| document_err(id, e))?;
        self.persist(id, &mut doc, &note)?;
        Ok(note.metadata)
    }

    /// Write the note's denormalized columns + saved document to SQLite and
    /// refresh the search index. Shared by [`mutate`](Self::mutate) and
    /// [`write_body`](Self::write_body).
    fn persist(
        &mut self,
        _id: &str,
        doc: &mut AutoCommit,
        note: &NoteDoc,
    ) -> Result<(), StoreError> {
        let m = &note.metadata;
        self.conn.execute(
            "UPDATE notes SET title = ?2, tags = ?3, pinned = ?4, deleted = ?5,
             has_todos = ?6, modified_at = ?7, note_type = ?8, status = ?9, doc = ?10 WHERE id = ?1",
            params![
                m.id,
                m.title,
                tags_json(&m.tags),
                m.pinned,
                m.deleted,
                content::has_unchecked_todos(note.body.as_str()),
                m.modified_at,
                m.note_type,
                m.status,
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
            Some(b) => AutoCommit::load(&b)
                .map(Some)
                .map_err(|e| document_err(id, e)),
        }
    }
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
