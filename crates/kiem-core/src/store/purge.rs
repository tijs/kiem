//! Permanent erasure: Empty Trash, deleting a project, and adopting a peer's
//! purges over sync.
//!
//! Distinct from `delete_note`, which only sets a flag. A purge deletes the
//! row *and* records the id in the `purged` table and in a synced CRDT
//! document, so a peer that still holds the note cannot resurrect it — see
//! the accept-and-drop check in `put_doc_impl`.

use automerge::AutoCommit;
use autosurgeon::{hydrate, reconcile};
use rusqlite::params;

use crate::sync::TOMBSTONES_DOC_ID;

use super::{document_err, NoteStore, StoreError, TombstoneDoc};

impl NoteStore {
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
            // Deferred, not immediate: a tombstone doc received mid-sync-burst
            // runs while this store's own deferred writer is already open, and
            // an immediate `remove_note` per id would burn the full writer-lock
            // retry budget (~2s) against our own open writer, per id, inside
            // the store mutex — 32 purged ids stalled sync for a measured 64+
            // seconds (finding baf2d005). The deferred call reuses the open
            // writer for free; the flush below keeps the local Empty Trash
            // path as immediate as before.
            for id in ids {
                if let Err(e) = index.remove_note_deferred(id) {
                    Self::log_index_failure(id, e);
                    // Writer unavailable: skip the rest of the batch instead
                    // of paying the retry budget per id. The index is derived
                    // (these entries were already removed at trash time), so
                    // missing removals cost nothing but staleness.
                    break;
                }
            }
            if let Err(e) = index.flush() {
                eprintln!("kiem: search index flush after purge failed, will retry on next write: {e}");
            }
        }
        Ok(erased)
    }
}
