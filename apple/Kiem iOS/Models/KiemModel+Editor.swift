import Foundation
import KiemKit

/// The editor buffer: the one piece of state Swift owns while a note is open,
/// and the debounced version-aware write-back that persists it. Mirrors the
/// macOS model's `KiemModel+Editor.swift`, minus the AppKit termination hook
/// (iOS persists pending edits via scene-phase handling in the App).
extension KiemModel {
    func editorTextDidChange() {
        guard let id = selectedNoteID, loadingNoteID == nil else { return }
        guard editorText != loadedBody, let version = loadedVersion else { return }
        pendingEdit = (noteID: id, text: editorText, expectedVersion: version)
        pendingEditTask?.cancel()
        pendingEditTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: Self.editDebounce)
            guard !Task.isCancelled else { return }
            self?.flushPendingEdit()
        }
    }

    func takePendingEdit() -> (noteID: String, text: String, expectedVersion: String)? {
        pendingEditTask?.cancel()
        pendingEditTask = nil
        guard let edit = pendingEdit else { return nil }
        pendingEdit = nil
        return edit
    }

    func flushPendingEdit() {
        guard let (id, text, version) = takePendingEdit() else { return }
        writingNoteID = id
        perform { try $0.updateNoteIfVersion(id: id, body: text, expectedVersion: version) } onFailure: {
            self.writingNoteID = nil
            self.rejectedEditorDraft = (noteID: id, text: text)
            self.errorMessage = "This note changed elsewhere, so your stale edit was not applied. The latest version was reloaded; the rejected draft remains available for conflict resolution."
            self.reloadEditorAfterRejectedWrite(noteID: id)
        } then: { note in
            self.writingNoteID = nil
            guard self.selectedNoteID == id else { return }
            self.loadedBody = note.body
            self.loadedVersion = note.version
            self.rejectedEditorDraft = nil
            self.refreshNotes()
            self.refreshSidebar()
        }
    }

    func flushPendingEditBlocking() {
        guard let (id, text, version) = takePendingEdit() else { return }
        writingNoteID = id
        let store = self.store
        let result = storeQueue.sync {
            Result { try store.updateNoteIfVersion(id: id, body: text, expectedVersion: version) }
        }
        switch result {
        case let .success(note):
            writingNoteID = nil
            rejectedEditorDraft = nil
            guard selectedNoteID == id else { return }
            loadedBody = note.body
            loadedVersion = note.version
        case let .failure(error):
            // Background flush of a stale edit: keep the user's draft visibly
            // available for conflict resolution instead of silently dropping
            // it, and surface the failure so it isn't lost when the process is
            // suspended. The latest body is reloaded whenever the editor still
            // points at this note.
            writingNoteID = nil
            rejectedEditorDraft = (noteID: id, text: text)
            errorMessage = "Your unsaved edit wasn't saved (\(error)). The latest version was reloaded and your rejected draft was kept for conflict resolution."
            reloadEditorAfterRejectedWrite(noteID: id)
        }
    }

    func loadSelectedNote() {
        flushPendingEdit()
        loadedBody = ""
        editorText = ""
        guard let id = selectedNoteID else {
            loadingNoteID = nil
            return
        }
        loadingNoteID = id
        perform { try $0.getNote(id: id) } onFailure: {
            self.loadingNoteID = nil
        } then: { note in
            guard self.loadingNoteID == id else { return }
            self.loadingNoteID = nil
            guard let note else { return }
            self.loadedBody = note.body
            self.loadedVersion = note.version
            self.editorText = note.body
        }
    }

    func reloadEditorAfterRejectedWrite(noteID: String) {
        guard selectedNoteID == noteID, loadingNoteID == nil else { return }
        perform { try $0.getNote(id: noteID) } then: { note in
            guard let note, self.selectedNoteID == noteID, self.writingNoteID == nil else { return }
            self.loadedBody = note.body
            self.loadedVersion = note.version
            self.editorText = note.body
        }
    }

    /// Clear the preserved rejected-draft marker. In this first slice the
    /// draft body itself is not re-applied to the buffer out of the box — the
    /// marker keeps the rejected draft observable so a version conflict is
    /// never silently dropped; re-installing the body is a documented follow-on.
    func discardRejectedDraft() {
        rejectedEditorDraft = nil
    }
}
