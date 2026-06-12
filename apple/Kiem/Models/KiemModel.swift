import Foundation
import KiemKit
import Observation

/// App-wide state over the Rust core. All reads go through `KiemStore`
/// (denormalized SQLite columns — cheap); the editor's text is the only
/// state Swift owns while a note is open.
@Observable @MainActor
final class KiemModel {
    /// Stand-in author until the identity module (U11) provides real DIDs.
    /// Matches the CLI so app- and CLI-created notes look the same.
    private static let authorPlaceholder = "local"

    private let store: KiemStore

    private(set) var notes: [NoteMetadata] = []
    private(set) var tags: [TagCount] = []
    var errorMessage: String?

    var selectedTag: String? {
        didSet { refreshNotes() }
    }

    var selectedNoteID: String? {
        didSet { loadSelectedNote() }
    }

    /// Editor binding for the selected note. While editing, this is the
    /// source of truth; the store mirrors it on every change.
    var editorText: String = ""

    init(dataDir: URL) throws {
        store = try KiemStore.open(dataDir: dataDir.path)
        refresh()
    }

    /// Default data directory — the same one the CLI uses, so the app and
    /// `kiem` work on one store (multi-process safe by design).
    /// `KIEM_DATA_DIR` overrides it for development and testing.
    static func defaultDataDir() -> URL {
        if let override = ProcessInfo.processInfo.environment["KIEM_DATA_DIR"] {
            return URL(fileURLWithPath: override)
        }
        return FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".kiem")
    }

    func refresh() {
        refreshNotes()
        tags = report { try store.getTags() } ?? []
    }

    func refreshNotes() {
        let listed: [NoteMetadata]? = report {
            if let tag = selectedTag {
                try store.listByTag(tag: tag)
            } else {
                try store.listNotes()
            }
        }
        notes = listed ?? []
        if let selected = selectedNoteID, !notes.contains(where: { $0.id == selected }) {
            selectedNoteID = nil
        }
    }

    var selectedNote: NoteMetadata? {
        notes.first { $0.id == selectedNoteID }
    }

    func createNote() {
        guard let meta = report({
            try store.createNote(body: "# ", authorDid: Self.authorPlaceholder)
        }) else { return }
        refresh()
        selectedNoteID = meta.id
    }

    func deleteNote(id: String) {
        report { try store.deleteNote(id: id) }
        if selectedNoteID == id {
            selectedNoteID = nil
        }
        refresh()
    }

    /// Editor change → Rust (re-derives title/tags) → refresh metadata.
    func editorTextDidChange() {
        guard let id = selectedNoteID else { return }
        report { try store.updateNote(id: id, body: editorText) }
        refreshNotes()
        tags = report { try store.getTags() } ?? []
    }

    private func loadSelectedNote() {
        guard let id = selectedNoteID,
              let note = report({ try store.getNote(id: id) }) ?? nil
        else {
            editorText = ""
            return
        }
        editorText = note.body
    }

    /// Run a store call, surfacing failures in the UI instead of crashing.
    @discardableResult
    private func report<T>(_ work: () throws -> T) -> T? {
        do {
            return try work()
        } catch {
            errorMessage = "\(error)"
            return nil
        }
    }
}
