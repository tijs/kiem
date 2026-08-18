import Foundation
import KiemKit
import Pulp

/// The editor buffer: the one piece of state Swift owns while a note is open,
/// and the debounced write-back that persists it. The buffer itself
/// (`editorText`, `loadedBody`, `pendingEdit`) is declared on the class in
/// `KiemModel.swift`; extensions cannot add stored properties.
extension KiemModel {
    /// Editor change → schedule a debounced store write (Rust re-derives
    /// title/tags at flush time, then metadata refreshes).
    func editorTextDidChange() {
        guard let id = selectedNoteID, loadingNoteID == nil else { return }
        // Loading a note assigns `editorText` programmatically, which also fires
        // this handler. Skip when nothing actually changed: otherwise every
        // note-open re-derives + persists metadata (bumping modified_at and, with
        // a mismatched embedded core, clobbering data). See
        // docs/solutions/integration-issues/stale-prebuilt-kiemkit-xcframework-clobbers-tags-2026-06-20.md
        guard editorText != loadedBody, let version = loadedVersion else { return }
        pendingEdit = (noteID: id, text: editorText, expectedVersion: version)
        pendingEditTask?.cancel()
        pendingEditTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: Self.editDebounce)
            guard !Task.isCancelled else { return }
            self?.flushPendingEdit()
        }
    }

    /// Take the pending edit, if any, and stop its debounce timer.
    func takePendingEdit() -> (noteID: String, text: String, expectedVersion: String)? {
        pendingEditTask?.cancel()
        pendingEditTask = nil
        guard let edit = pendingEdit else { return nil }
        pendingEdit = nil
        return edit
    }

    /// Queue the pending edit's write (if any) and refresh derived state. The
    /// write is *enqueued* synchronously, so anything queued after it — a tag
    /// add, a delete, the next note's load — still lands after it.
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

    /// Persist the pending edit *before returning*. Only for app termination,
    /// where an enqueued write would die with the process. Blocks the main
    /// thread by design — at quit that's a beat, not a beachball.
    func flushPendingEditBlocking() {
        guard let (id, text, version) = takePendingEdit() else { return }
        let store = self.store
        storeQueue.sync { try? store.updateNoteIfVersion(id: id, body: text, expectedVersion: version) }
    }

    func loadSelectedNote() {
        // Selection already points at the new note; this persists the
        // previous note's still-pending text before its editor state goes away.
        flushPendingEdit()
        // Blank the editor for the duration of the fetch rather than leaving the
        // previous note's text under the new note's id. Setting `loadedBody`
        // first keeps the change handler the assignment triggers a no-op.
        loadedBody = ""
        editorText = ""
        guard let id = selectedNoteID else {
            loadingNoteID = nil
            return
        }
        loadingNoteID = id
        // On failure the editor stays blank, but must not stay uneditable —
        // a stuck `loadingNoteID` would swallow every keystroke silently.
        perform { try $0.getNote(id: id) } onFailure: {
            self.loadingNoteID = nil
        } then: { note in
            // A newer load (or a cleared selection) supersedes this one.
            guard self.loadingNoteID == id else { return }
            self.loadingNoteID = nil
            guard let note else { return }
            self.loadedBody = note.body
            self.loadedVersion = note.version
            self.editorText = note.body
        }
    }

    /// Conflict recovery deliberately reloads rather than retrying the stale
    /// whole-body replacement. Retrying would turn a rejected debounce into the
    /// same cross-process clobber the version check prevented.
    func reloadEditorAfterRejectedWrite(noteID: String) {
        guard selectedNoteID == noteID, loadingNoteID == nil else { return }
        perform { try $0.getNote(id: noteID) } then: { note in
            guard let note, self.selectedNoteID == noteID, self.writingNoteID == nil else { return }
            self.loadedBody = note.body
            self.loadedVersion = note.version
            self.editorText = note.body
        }
    }

    /// Serial queue for every FFI call. The Rust core serializes store access
    /// behind one mutex, and the sync tick can hold it for seconds on a large
    /// store — so calling it from `@MainActor` beachballs the app. Work goes
    /// here; results are applied back on the main actor. Both queues are FIFO,
    /// so calls land — and their results apply — in issue order, the same
    /// strict ordering the old synchronous calls had (Swift actors give no
    /// such guarantee).
}

/// Tapping a `#hashtag` in the editor selects it in the sidebar, mirroring the
/// project breadcrumb's `openProject`. Edits do *not* come through
/// `didApplyEdit` — the app tracks them via the `$model.editorText` binding —
/// but the protocol requires the method, so it stays a no-op.
extension KiemModel: PulpEditorDelegate {
    func editor(_ editor: PulpEditorProtocol, didApplyEdit edit: TextEdit) {}

    func editor(_ editor: PulpEditorProtocol, didTapHashtag tag: String) {
        selection = tag.hasPrefix(Self.projectTagPrefix) ? .project(tag) : .tag(tag)
    }
}
