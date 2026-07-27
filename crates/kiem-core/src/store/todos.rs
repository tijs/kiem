//! Checkbox operations. Each one reads the body, edits it with
//! `crate::content`, and goes back through `update_note` — so a todo toggle
//! is an ordinary body edit as far as storage and sync are concerned, and
//! title/tag derivation stays in the one place that owns it.

use crate::content;
use crate::note::NoteMetadata;

use super::{document_err, NoteStore, StoreError};

impl NoteStore {
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
}
