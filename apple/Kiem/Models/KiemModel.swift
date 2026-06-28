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
    /// Tags excluding the `proj/*` namespace (those surface under Projects).
    private(set) var tags: [TagCount] = []
    /// Projects, derived from `proj/*` tags with their note counts.
    private(set) var projects: [TagCount] = []
    /// Open todos for the selected project (empty unless viewing one).
    private(set) var projectTodos: [ProjectTodo] = []
    /// Live match counts per smart filter, shown beside its sidebar row.
    private(set) var filterCounts: [SmartFilter: Int] = [:]
    /// Peers currently linked for sync (drives the sync-status UI in U13).
    private(set) var connectedPeers: [ConnectedPeer] = []
    var errorMessage: String?

    private var peerManager: PeerManager?

    var selection: SidebarSelection = .allNotes {
        didSet { refreshNotes() }
    }

    /// Full-text search query. Non-empty overrides the sidebar selection and
    /// drives the note list from the Rust core's search index.
    var searchText: String = "" {
        didSet { refreshNotes() }
    }

    /// Whether the trash filter is the active selection (gates restore vs. trash).
    var isViewingTrash: Bool {
        if case .filter(.trash) = selection { return true }
        return false
    }

    /// Whether a project is the active selection (gates the project todo panel).
    var isViewingProject: Bool {
        if case .project = selection { return true }
        return false
    }

    /// Empty-list heading for the current selection.
    var emptyNotesTitle: String {
        if !searchText.isEmpty { return "No matches for “\(searchText)”" }
        switch selection {
        case .allNotes: return "No notes yet"
        case let .tag(tag): return "No notes tagged #\(tag)"
        case let .project(tag): return "No notes in \(Self.projectName(tag))"
        case let .filter(filter): return filter.emptyTitle
        }
    }

    /// Display name for a project tag: `proj/kiem_app` → `kiem_app`.
    static func projectName(_ tag: String) -> String {
        tag.hasPrefix("proj/") ? String(tag.dropFirst("proj/".count)) : tag
    }

    var selectedNoteID: String? {
        didSet { loadSelectedNote() }
    }

    /// Editor binding for the selected note. While editing, this is the
    /// source of truth; the store mirrors it on every change.
    var editorText: String = ""

    /// The body last loaded into the editor. Lets `editorTextDidChange`
    /// distinguish a programmatic load (no write needed) from a real user edit.
    private var loadedBody = ""

    init(dataDir: URL) throws {
        store = try KiemStore.open(dataDir: dataDir.path)
        refresh()
        startSync()
    }

    /// Begin P2P sync: discover peers on the local network and converge notes,
    /// the same Bonjour service (`_kiem._tcp`) the CLI daemon uses.
    private func startSync() {
        let manager = PeerManager(store: store, localPeerId: Self.peerID())
        manager.onPeersChanged = { [weak self] peers in
            Task { @MainActor in self?.connectedPeers = peers }
        }
        manager.start()
        peerManager = manager
    }

    /// Stable peer id for this install. `KIEM_PEER_ID` overrides it so several
    /// instances can run on one machine during development (otherwise they'd
    /// share UserDefaults, see the same id, and treat each other as self).
    private static func peerID() -> String {
        if let override = ProcessInfo.processInfo.environment["KIEM_PEER_ID"], !override.isEmpty {
            return override
        }
        let key = "org.tijs.kiem.peerID"
        if let existing = UserDefaults.standard.string(forKey: key) { return existing }
        let fresh = UUID().uuidString
        UserDefaults.standard.set(fresh, forKey: key)
        return fresh
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

    func refreshNotes() {
        let listed: [NoteMetadata]? = searchText.isEmpty
            ? report { try notes(for: selection) }
            : report { try searchResults(matching: searchText) }
        notes = listed ?? []
        if let selected = selectedNoteID, !notes.contains(where: { $0.id == selected }) {
            selectedNoteID = nil
        }
        refreshProjectTodos()
    }

    /// Load (or clear) the selected project's open todos.
    private func refreshProjectTodos() {
        guard case let .project(tag) = selection, searchText.isEmpty else {
            projectTodos = []
            return
        }
        projectTodos = report { try store.listTodoItemsForTag(tag: tag) } ?? []
    }

    /// Toggle a project todo by its (note, index) address and refresh.
    func toggleProjectTodo(noteID: String, index: UInt32, checked: Bool) {
        report { try store.setTodoChecked(noteId: noteID, index: index, checked: checked) }
        refresh()
    }

    /// Full-text search via the Rust core, mapped back to list metadata with
    /// rank order preserved. Trashed hits drop out — they're not in `listNotes`.
    private func searchResults(matching query: String) throws -> [NoteMetadata] {
        let hits = try store.search(query: query, limit: 50)
        let byID = Dictionary(uniqueKeysWithValues: try store.listNotes().map { ($0.id, $0) })
        return hits.compactMap { byID[$0.noteId] }
    }

    /// The note list backing a sidebar selection. Each case maps to a dedicated
    /// `KiemStore` query; the filtering itself lives in the Rust core.
    private func notes(for selection: SidebarSelection) throws -> [NoteMetadata] {
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

    /// Refresh the sidebar's tag list, project list, and smart-filter counts.
    private func refreshSidebar() {
        let allTags = report { try store.getTags() } ?? []
        projects = allTags.filter { $0.tag.hasPrefix("proj/") }
        tags = allTags.filter { !$0.tag.hasPrefix("proj/") }
        filterCounts = SmartFilter.allCases.reduce(into: [:]) { counts, filter in
            counts[filter] = report { try notes(for: .filter(filter)).count } ?? 0
        }
    }

    /// Create a new project: a home note carrying the `proj/<slug>` tag so the
    /// project appears in the synced store. (The committed `.kiem` repo marker is
    /// the CLI/agent's responsibility, not the app's.)
    func createProject(name: String) {
        let tag = Self.projectTag(for: name)
        guard !tag.isEmpty else { return }
        report { try store.createNote(body: "# \(name)\n\nProject home.\n\n#\(tag)", authorDid: Self.authorPlaceholder) }
        refresh()
        selection = .project(tag)
    }

    /// `proj/<slug>` from a free-form name. Mirrors the Rust CLI slug rule in
    /// `crates/kiem-cli/src/project.rs` (`slugify`): lowercase; space/`-`/`_` → `_`;
    /// keep `[a-z0-9/]`; collapse repeats; trim `_`. The `_` separator matters —
    /// the tag grammar rejects `-`, so a `-` slug would not round-trip.
    static func projectTag(for name: String) -> String {
        var slug = ""
        var prevSep = false
        for ch in name.lowercased() {
            if ch.isASCII && (ch.isLetter || ch.isNumber || ch == "/") {
                slug.append(ch)
                prevSep = false
            } else if ch == " " || ch == "-" || ch == "_" {
                if !prevSep && !slug.isEmpty {
                    slug.append("_")
                    prevSep = true
                }
            }
        }
        while slug.hasSuffix("_") { slug.removeLast() }
        return slug.isEmpty ? "" : "proj/\(slug)"
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

    /// Restore a trashed note (undo a "Move to Trash").
    func restoreNote(id: String) {
        report { try store.restoreNote(id: id) }
        refresh()
    }

    /// Editor change → Rust (re-derives title/tags) → refresh metadata.
    func editorTextDidChange() {
        guard let id = selectedNoteID else { return }
        // Loading a note assigns `editorText` programmatically, which also fires
        // this handler. Skip when nothing actually changed: otherwise every
        // note-open re-derives + persists metadata (bumping modified_at and, with
        // a mismatched embedded core, clobbering data). See
        // docs/solutions/integration-issues/stale-prebuilt-kiemkit-xcframework-clobbers-tags-2026-06-20.md
        guard editorText != loadedBody else { return }
        loadedBody = editorText
        report { try store.updateNote(id: id, body: editorText) }
        refreshNotes()
        refreshSidebar()
    }

    private func loadSelectedNote() {
        guard let id = selectedNoteID,
              let note = report({ try store.getNote(id: id) }) ?? nil
        else {
            loadedBody = ""
            editorText = ""
            return
        }
        // Set `loadedBody` before `editorText` so the change handler the
        // assignment triggers sees them equal and skips the write.
        loadedBody = note.body
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
