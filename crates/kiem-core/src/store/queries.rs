//! Read-side queries: listings, counts, search, tags, and project todos.
//! Split out of `store/mod.rs` (file-size limit); same `NoteStore`, no
//! behavior of its own — every query reads the denormalized columns the
//! write path maintains.

use autosurgeon::hydrate;
use rusqlite::{params, OptionalExtension, Row};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::{doc_version, document_err, FilterCounts, NoteStore, ProjectTodo, StoreError};
use crate::content;
use crate::note::{NoteDoc, NoteMetadata};
use crate::search::SearchResult;

const META_COLUMNS: &str =
    "id, title, tags, author_did, note_type, pinned, deleted, created_at, modified_at, status";

impl NoteStore {
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

    /// A short, content-addressed version token for a note (its Automerge
    /// document heads, hex). Read it alongside a note, pass it back as
    /// `expect_version` to [`edit_lines`](Self::edit_lines) to reject an edit if
    /// the note changed underneath you (the todo-index / concurrent-edit race).
    pub fn note_version(&self, id: &str) -> Result<String, StoreError> {
        let mut doc = self.load_doc(id)?.ok_or_else(|| StoreError::NotFound(id.to_owned()))?;
        Ok(doc_version(&mut doc))
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
        self.list_modified_on(&today_utc())
    }

    /// Match counts for every smart filter in one table scan — the sidebar
    /// needs only the numbers, not five materialized lists. Predicates are
    /// exactly the ones the corresponding `list_*` queries use.
    pub fn filter_counts(&self) -> Result<FilterCounts, StoreError> {
        Ok(self.conn.query_row(
            "SELECT
               COUNT(*) FILTER (WHERE deleted = 0 AND has_todos = 1),
               COUNT(*) FILTER (WHERE deleted = 0 AND substr(modified_at, 1, 10) = ?1),
               COUNT(*) FILTER (WHERE deleted = 0 AND tags = '[]'),
               COUNT(*) FILTER (WHERE deleted = 0 AND pinned = 1),
               COUNT(*) FILTER (WHERE deleted = 1)
             FROM notes",
            params![today_utc()],
            |r| {
                let count = |i: usize| -> rusqlite::Result<u64> { Ok(r.get::<_, i64>(i)? as u64) };
                Ok(FilterCounts {
                    todo: count(0)?,
                    today: count(1)?,
                    untagged: count(2)?,
                    pinned: count(3)?,
                    trash: count(4)?,
                })
            },
        )?)
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
        status: row.get("status")?,
    })
}

/// Today's UTC calendar date, `YYYY-MM-DD`.
fn today_utc() -> String {
    let now = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("RFC 3339 formatting of a valid UTC time cannot fail");
    now[..10].to_owned()
}
