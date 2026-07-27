import Foundation
import KiemKit

/// Multi-select actions from the note list's context menu and drag-to-sidebar.
/// Each applies to a set of ids and refreshes once at the end rather than per
/// note.
extension KiemModel {
    // MARK: Bulk actions (multi-select context menu + drag to sidebar)

    /// Move notes to trash. One action for a single note or a selection.
    func trashNotes(_ ids: Set<String>) {
        // Don't let a pending edit land in a note after it's trashed.
        flushPendingEdit()
        let replacementID: String?
        if ids.count == 1,
           let selected = selectedNoteID,
           ids.contains(selected),
           let index = notes.firstIndex(where: { $0.id == selected }) {
            replacementID = notes.dropFirst(index + 1).first?.id ?? notes[..<index].last?.id
        } else {
            replacementID = nil
        }
        perform { store in
            for id in ids { _ = try store.deleteNote(id: id) }
        } then: { _ in
            self.selectedNoteIDs.subtract(ids)
            self.refreshSidebar()
            self.refreshNotes {
                if let replacementID, self.notes.contains(where: { $0.id == replacementID }) {
                    self.selectedNoteID = replacementID
                }
            }
        }
    }

    /// Restore trashed notes (undo "Move to Trash").
    func restoreNotes(_ ids: Set<String>) {
        perform { store in
            for id in ids { _ = try store.restoreNote(id: id) }
        } then: { _ in self.refresh() }
    }

    /// Pin or unpin notes.
    func setPinned(_ ids: Set<String>, pinned: Bool) {
        perform { store in
            for id in ids { _ = try store.setPinned(id: id, pinned: pinned) }
        } then: { _ in self.refresh() }
    }

    /// Add a hashtag to notes — both "tag it" (plain tag) and "add to
    /// project" (a `proj/<slug>` tag) are this one operation. Appends to the
    /// body unless already present, so it's the same sync-safe body-update
    /// path as typing the tag; the open note reloads to show it.
    func addTag(_ ids: Set<String>, tag: String) {
        // A pending edit flushed later would overwrite the tagged body.
        flushPendingEdit()
        perform { store in
            for id in ids { _ = try store.addTag(id: id, tag: tag) }
        } then: { _ in
            self.refresh()
            if let selected = self.selectedNoteID, ids.contains(selected) {
                self.loadSelectedNote()
            }
        }
    }
}
