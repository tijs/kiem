//! How a write actually lands: the sync-receive path (`put_doc`) and the
//! local-edit path (`mutate` / `write_body`), plus the shared `persist` that
//! recomputes every denormalized column and the search index from the
//! document.
//!
//! Every mutation in the store funnels through here, and that is the point:
//! the columns are an index over the Automerge document, never an authority,
//! and this is the one place that invariant is maintained.

use automerge::AutoCommit;
use autosurgeon::{hydrate, reconcile};
use rusqlite::params;

use crate::content;
use crate::note::{NoteDoc, NoteMetadata};

use super::{body_obj, document_err, now_rfc3339, tags_json, NoteStore, StoreError};

impl NoteStore {
    /// Persist a document that changed outside the normal edit path (sync
    /// receive). Inserts or fully replaces the row from the document's own
    /// hydrated state and keeps the search index in step.
    pub fn put_doc(&mut self, doc: &mut AutoCommit) -> Result<NoteMetadata, StoreError> {
        self.put_doc_impl(doc, false)
    }

    /// Like `put_doc`, but defers the search-index write instead of
    /// committing it immediately — for a caller applying many documents in
    /// a burst (sync receiving a bulk resync), which would otherwise pay a
    /// full tantivy commit+reload per document. The caller must eventually
    /// call `flush_search_index` (the sync ticker does this once per tick);
    /// until then the note is persisted and syncs correctly, it just isn't
    /// searchable yet.
    pub fn put_doc_deferred(&mut self, doc: &mut AutoCommit) -> Result<NoteMetadata, StoreError> {
        self.put_doc_impl(doc, true)
    }

    fn put_doc_impl(&mut self, doc: &mut AutoCommit, defer_index: bool) -> Result<NoteMetadata, StoreError> {
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
            let result = match (m.deleted, defer_index) {
                (true, true) => index.remove_note_deferred(&m.id),
                (true, false) => index.remove_note(&m.id),
                (false, true) => index.index_note_deferred(m, note.body.as_str()),
                (false, false) => index.index_note(m, note.body.as_str()),
            };
            if let Err(e) = result {
                Self::log_index_failure(&m.id, e);
            }
        }
        Ok(note.metadata)
    }

    /// Commits any search-index writes deferred by `put_doc_deferred`. A
    /// cheap no-op when nothing is pending.
    pub fn flush_search_index(&mut self) -> Result<(), StoreError> {
        if let Some(index) = &mut self.search {
            index.flush()?;
        }
        Ok(())
    }

    // -- internals --

    /// Hydrate → mutate → reconcile into the *same* document, then persist.
    /// The store owns the document for the whole cycle, the single-connection
    /// equivalent of the U6 rule that edits and sync-receives serialize per
    /// document (autosurgeon StaleHeads).
    pub(super) fn mutate(
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
    pub(super) fn write_body(&mut self, id: &str, new_body: &str) -> Result<NoteMetadata, StoreError> {
        self.write_body_inner(id, new_body, false)
    }

    pub(super) fn write_body_inner(
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
    pub(super) fn persist(
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
            let result = if m.deleted {
                index.remove_note(&m.id)
            } else {
                index.index_note(m, note.body.as_str())
            };
            if let Err(e) = result {
                Self::log_index_failure(&m.id, e);
            }
        }
        Ok(())
    }

    pub(super) fn load_doc(&self, id: &str) -> Result<Option<AutoCommit>, StoreError> {
        match self.get_doc_bytes(id)? {
            None => Ok(None),
            Some(b) => AutoCommit::load(&b)
                .map(Some)
                .map_err(|e| document_err(id, e)),
        }
    }
}
