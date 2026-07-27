import AppKit
import Foundation
import KiemKit

/// Keeping the UI's copy of the store in step: the file watcher that notices
/// writes from outside this process (the `kiem` CLI, or incoming P2P sync),
/// and the reload paths every mutation ends in.
extension KiemModel {
    /// Watch the shared SQLite store for writes from outside our own mutation
    /// calls: an external `kiem` CLI process, or an incoming P2P sync applied by
    /// the Rust mesh (`kiem-sync`). Both land in the same on-disk file (WAL
    /// mode — `crates/kiem-core/src/store/`), so one watcher covers both; no
    /// per-note sync callback is needed. Debounced so a burst of writes
    /// triggers one refresh, not one per write — the app's own writes
    /// harmlessly retrigger a refresh too, which isn't worth special-casing away.
    func watchStoreForExternalWrites(dataDir: URL) {
        let paths = ["kiem.db", "kiem.db-wal"].map { dataDir.appendingPathComponent($0).path }
        dbWatchSources = paths.compactMap { path in
            let fd = open(path, O_EVTONLY)
            guard fd >= 0 else { return nil }
            let source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: fd, eventMask: .write, queue: .main)
            source.setEventHandler { [weak self] in
                Task { @MainActor in self?.scheduleDebouncedRefresh() }
            }
            source.setCancelHandler { close(fd) }
            source.resume()
            return source
        }
    }

    func scheduleDebouncedRefresh() {
        pendingRefreshTask?.cancel()
        pendingRefreshTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled, let self, self.activeTransfer == nil else { return }
            self.refresh()
            self.reloadEditorIfExternalWriteChangedIt()
        }
    }

    /// If a sync (or other external) write changed the open note while the
    /// editor was holding stale text, reload the editor so the next flush does
    /// not clobber the incoming change. Active typing is left alone: the pending
    /// edit will merge with the external change on flush.
    func reloadEditorIfExternalWriteChangedIt() {
        guard let id = selectedNoteID, pendingEditTask == nil, loadingNoteID == nil else { return }
        perform { try $0.getNote(id: id) } then: { note in
            // Re-check: the user may have switched notes or resumed typing while
            // the read was queued.
            guard let note, self.selectedNoteID == id, self.pendingEditTask == nil,
                  self.loadingNoteID == nil, note.body != self.editorText
            else { return }
            // The pending edit (if any) targets the stale body; drop it.
            self.pendingEdit = nil
            self.loadedBody = note.body
            self.editorText = note.body
        }
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
        refreshSidebar()
    }

    /// Reload the note list for the current selection/search. `completion` runs
    /// on the main actor once the new list is applied — the hook for actions
    /// that must select a note the refresh has to know about first.
    func refreshNotes(then completion: (@Sendable @MainActor () -> Void)? = nil) {
        // Captured, not read again in the completion: the selection may have
        // moved on while the query was queued, and applying a list built for
        // the old one would show the wrong notes.
        let selection = self.selection
        let query = searchText
        // A failed query leaves notes + selection untouched — clearing on a
        // transient DB error (e.g. SQLITE_BUSY racing a sync write) blanks the
        // editor even though the note is intact in the store. `perform` skips
        // `apply` on failure, so that falls out for free.
        perform { store in
            query.isEmpty
                ? try Self.notes(for: selection, in: store)
                : try Self.searchResults(matching: query, in: store)
        } then: { listed in
            guard self.selection == selection, self.searchText == query else { return }
            self.notes = listed
            // Prune selected notes that left the current view (trashed, filtered
            // out, edited elsewhere) — for a single selection this also clears
            // the editor via the selection didSet.
            let visible = Set(listed.map(\.id))
            if !self.selectedNoteIDs.isSubset(of: visible) {
                self.selectedNoteIDs.formIntersection(visible)
            }
            self.refreshOpenTodos()
            completion?()
        }
    }

    /// Load (or clear) the open todos for the current view: the selected
    /// project's, or every note's when the Todo smart filter is active.
    func refreshOpenTodos() {
        guard searchText.isEmpty else {
            openTodos = []
            return
        }
        switch selection {
        case let .project(tag):
            perform { try $0.listTodoItemsForTag(tag: tag) } then: { self.openTodos = $0 }
        case .filter(.todo):
            perform { try $0.listOpenTodoItems() } then: { self.openTodos = $0 }
        default:
            openTodos = []
        }
    }

    /// Refresh the sidebar's tag list, project list, and smart-filter counts.
    /// One store call for both, so the counts can't disagree with the tags.
    func refreshSidebar() {
        perform { store in
            try (tags: store.getTags(), counts: store.filterCounts())
        } then: { result in
            self.projects = result.tags.filter { $0.tag.hasPrefix(Self.projectTagPrefix) }
            self.tags = result.tags.filter { !$0.tag.hasPrefix(Self.projectTagPrefix) }
            self.filterCounts = [
                .todo: Int(result.counts.todo),
                .today: Int(result.counts.today),
                .untagged: Int(result.counts.untagged),
                .pinned: Int(result.counts.pinned),
                .trash: Int(result.counts.trash)
            ]
        }
    }

    /// Create a new project: a home note carrying the `proj/<slug>` tag so the

    /// Full-text search via the Rust core, mapped back to list metadata with
    /// rank order preserved. Trashed hits drop out — they're not in `listNotes`.
    nonisolated static func searchResults(
        matching query: String, in store: KiemStore
    ) throws -> [NoteMetadata] {
        let hits = try store.search(query: query, limit: 50)
        let byID = try Dictionary(uniqueKeysWithValues: store.listNotes().map { ($0.id, $0) })
        return hits.compactMap { byID[$0.noteId] }
    }

    /// The note list backing a sidebar selection. Each case maps to a dedicated
    /// `KiemStore` query; the filtering itself lives in the Rust core.
    nonisolated static func notes(
        for selection: SidebarSelection, in store: KiemStore
    ) throws -> [NoteMetadata] {
        switch selection {
        case .allNotes: try store.listNotes()
        case let .tag(tag): try store.listByTag(tag: tag)
        case let .project(tag): try store.listByTag(tag: tag)
        case .filter(.todo): try store.listTodos()
        case .filter(.today): try store.listToday()
        case .filter(.untagged): try store.listUntagged()
        case .filter(.pinned): try store.listPinned()
        case .filter(.trash): try store.listDeleted()
        }
    }

    /// project appears in the synced store. (The committed `.kiem` repo marker is
    /// the CLI/agent's responsibility, not the app's.)
}
