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
    /// Ids of peers currently linked for sync (drives the sync-status UI in U13).
    private(set) var connectedPeers: [String] = []
    var errorMessage: String?
    /// Watches `kiem.db`/`kiem.db-wal` for writes from outside our own mutation
    /// calls (an external `kiem` CLI process, or incoming P2P sync). See
    /// `watchStoreForExternalWrites`.
    /// `nonisolated(unsafe)`: only ever mutated on the main actor, but `cancel()`
    /// is thread-safe and needs to run from `deinit`, which is nonisolated.
    private nonisolated(unsafe) var dbWatchSources: [DispatchSourceFileSystemObject] = []
    private nonisolated(unsafe) var pendingRefreshTask: Task<Void, Never>?

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

    /// The reserved namespace that makes a tag a project. Single source of truth
    /// (mirrors `TAG_PREFIX` in crates/kiem-cli/src/project.rs).
    static let projectTagPrefix = "proj/"

    /// Display name for a project tag: `proj/kiem_app` → `kiem_app`.
    static func projectName(_ tag: String) -> String {
        tag.hasPrefix(projectTagPrefix) ? String(tag.dropFirst(projectTagPrefix.count)) : tag
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
        watchStoreForExternalWrites(dataDir: dataDir)
    }

    deinit {
        for source in dbWatchSources {
            source.cancel()
        }
        pendingRefreshTask?.cancel()
        store.stopSync()
    }

    /// Begin P2P sync: join the iroh mesh in the Rust core (`kiem-sync`), the
    /// same transport and known-peers store the CLI daemon uses. Peers are
    /// added by ticket pairing (`kiem pair`); incoming sync writes land in the
    /// shared SQLite store, where the DB watcher picks them up for the UI.
    private func startSync() {
        let events = SyncPeerEvents { [weak self] peerId, connected in
            Task { @MainActor in
                guard let self else { return }
                var peers = Set(self.connectedPeers)
                if connected { peers.insert(peerId) } else { peers.remove(peerId) }
                self.connectedPeers = peers.sorted()
            }
        }
        let store = self.store
        // Off the main actor: binding the endpoint touches the network.
        Task.detached {
            do {
                try store.startSync(intervalMs: 1000, events: events)
            } catch {
                debugPrint("kiem sync failed to start: \(error)")
            }
        }
    }

    /// Watch the shared SQLite store for writes from outside our own mutation
    /// calls: an external `kiem` CLI process, or an incoming P2P sync applied by
    /// `PeerConnection.syncRound()`. Both land in the same on-disk file (WAL
    /// mode — `crates/kiem-core/src/store.rs`), so one watcher covers both; no
    /// need to also wire a callback through `PeerConnection`. Debounced so a
    /// burst of writes triggers one refresh, not one per write — the app's own
    /// writes harmlessly retrigger a refresh too, which isn't worth special-casing away.
    private func watchStoreForExternalWrites(dataDir: URL) {
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

    private func scheduleDebouncedRefresh() {
        pendingRefreshTask?.cancel()
        pendingRefreshTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled else { return }
            self?.refresh()
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

    func refreshNotes() {
        let listed: [NoteMetadata]? = searchText.isEmpty
            ? report { try notes(for: selection) }
            : report { try searchResults(matching: searchText) }
        // A failed query (listed == nil) leaves notes + selection untouched —
        // clearing on a transient DB error (e.g. SQLITE_BUSY racing a sync write)
        // blanks the editor even though the note is intact in the store.
        guard let listed else { return }
        notes = listed
        if let selected = selectedNoteID, !listed.contains(where: { $0.id == selected }) {
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
        // If the toggled note is open in the editor, re-sync its text. Otherwise
        // the editor keeps the pre-toggle body and the next keystroke writes it
        // back, silently reverting the checkbox. loadSelectedNote sets loadedBody
        // before editorText, so the change it triggers sees them equal and skips.
        if noteID == selectedNoteID { loadSelectedNote() }
    }

    /// Full-text search via the Rust core, mapped back to list metadata with
    /// rank order preserved. Trashed hits drop out — they're not in `listNotes`.
    private func searchResults(matching query: String) throws -> [NoteMetadata] {
        let hits = try store.search(query: query, limit: 50)
        let byID = try Dictionary(uniqueKeysWithValues: store.listNotes().map { ($0.id, $0) })
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
        projects = allTags.filter { $0.tag.hasPrefix(Self.projectTagPrefix) }
        tags = allTags.filter { !$0.tag.hasPrefix(Self.projectTagPrefix) }
        filterCounts = SmartFilter.allCases.reduce(into: [:]) { counts, filter in
            counts[filter] = report { try notes(for: .filter(filter)).count } ?? 0
        }
    }

    /// Create a new project: a home note carrying the `proj/<slug>` tag so the
    /// project appears in the synced store. (The committed `.kiem` repo marker is
    /// the CLI/agent's responsibility, not the app's.)
    func createProject(name: String) {
        let tag = Self.projectTag(for: name)
        guard !tag.isEmpty else {
            errorMessage = "Couldn’t make a project name from “\(name)”. Use letters or numbers."
            return
        }
        report { try store.createNote(body: "# \(name)\n\nProject home.\n\n#\(tag)", authorDid: Self.authorPlaceholder) }
        refresh()
        selection = .project(tag)
    }

    /// `proj/<slug>` from a free-form name. Byte-for-byte mirror of the Rust CLI
    /// `to_tag`/`slugify` in `crates/kiem-cli/src/project.rs`, enforced by the
    /// shared `fixtures/project-slug.json` parity contract: strip a leading
    /// `proj/`; lowercase ASCII A–Z only (non-ASCII is dropped, NOT Unicode-folded
    /// — `String.lowercased()` would diverge); keep `[a-z0-9/]`; space/`-`/`_` → a
    /// single `_`; collapse repeats; trim `_`. Empty slug → empty tag.
    static func projectTag(for name: String) -> String {
        let raw = name.hasPrefix(projectTagPrefix) ? String(name.dropFirst(projectTagPrefix.count)) : name
        var slug = ""
        var prevSep = false
        for ch in raw {
            // Mirror Rust's `to_ascii_lowercase`: only A–Z fold; everything else
            // is left as-is and then dropped if non-ASCII.
            let c: Character
            if let a = ch.asciiValue, (65 ... 90).contains(a) {
                c = Character(UnicodeScalar(a + 32))
            } else {
                c = ch
            }
            if let a = c.asciiValue, (97 ... 122).contains(a) || (48 ... 57).contains(a) || c == "/" {
                slug.append(c)
                prevSep = false
            } else if c == " " || c == "-" || c == "_" {
                if !prevSep && !slug.isEmpty {
                    slug.append("_")
                    prevSep = true
                }
            }
        }
        while slug.hasSuffix("_") {
            slug.removeLast()
        }
        return slug.isEmpty ? "" : projectTagPrefix + slug
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

/// Bridges `kiem-sync` mesh callbacks (arriving on Rust threads) to one
/// Sendable closure; the model hops them onto the main actor.
private final class SyncPeerEvents: PeerEvents {
    private let onChange: @Sendable (_ peerId: String, _ connected: Bool) -> Void

    init(onChange: @escaping @Sendable (_ peerId: String, _ connected: Bool) -> Void) {
        self.onChange = onChange
    }

    func onConnected(peerId: String) {
        onChange(peerId, true)
    }

    func onDisconnected(peerId: String) {
        onChange(peerId, false)
    }
}
