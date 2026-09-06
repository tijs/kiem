import Foundation
import KiemKit
import Observation

/// App-wide state over the Rust core, iOS edition. All reads go through the
/// shared `KiemStore` (denormalized SQLite + Automerge via `kiem-core`); the
/// editor's text is the only state Swift owns while a note is open.
///
/// This is the platform-neutral adaptation of the macOS `Kiem` target's
/// `KiemModel` (which is indissolubly bound to AppKit file watching, menus and
/// pasteboard). It shares the store-open, note-query, sidebar-grouping, async
/// refresh, debounced version-aware editing and sync-lifecycle behaviour, but
/// drops the AppKit-only bits. The two models are separate files by design so
/// neither change can destabilize the other target's build.
@Observable @MainActor
final class KiemModel {
    let store: KiemStore
    let authorDid: String
    let dataDir: URL

    var notes: [NoteMetadata] = []
    var tags: [TagCount] = []
    var projects: [TagCount] = []
    var openTodos: [ProjectTodo] = []
    var filterCounts: [SmartFilter: Int] = [:]

    // MARK: Pairing / sync state
    var connectedPeers: [String] = []
    var knownPeers: [String] = []
    var lastSyncActivity: [String: Date] = [:]
    /// Whether the Rust-backed sync mesh is armed. `startSync`/`stopSync` are
    /// idempotent around this flag; the scene-phase handler relies on it to
    /// restart the mesh on return-to-active after stopping it on background.
    /// Plain (not `private(set)`) because `startSync`/`stopSync` live in the
    /// `KiemModel+Sync` extension across files.
    var isSyncRunning = false
    var deviceName: String = ""
    var pairingTicket: String?
    var pairingWindowRemaining: Int?
    var pairingRequest: PairingRequest?
    /// Generation counter for pairing-window arms. `armPairingWindow` (and
    /// `closePairingWindow`, which invalidates) bump it so a detached ticket
    /// fetch that started under a superseded window is dropped on arrival. This
    /// is what stops an old arm from resurrecting a code after the sheet was
    /// closed or re-armed — a stale ticket must never reach `pairingTicket`.
    /// Plain (not `private(set)`) because the lifecycle lives in the
    /// `KiemModel+Sync` extension across files.
    var pairingGeneration: UInt64 = 0
    /// Whether the open sheet currently *wants* a discoverable pairing window.
    /// Set on arm, cleared once the store confirms the window is actually live
    /// (and on close). It lets `refreshPairingWindow` keep re-arming while the
    /// Rust mesh is still starting — where `arm_pairing` is a no-op — but stop
    /// the moment the window is genuinely armed, so a later expiry still falls
    /// through to the "make discoverable again" button instead of the sheet
    /// auto-refreshing an expired window forever.
    var wantsPairingWindow = false

    /// Serializes the Rust mesh start/stop lifecycle so a detached (queued) arm
    /// can never race a background stop, and a stale start can't tear down a
    /// newer mesh. Used by `startSync`/`stopSync` in `KiemModel+Sync.swift`
    /// (a separate file, hence not `private`).
    let syncLifecycleQueue = DispatchQueue(label: "org.kiem.sync.lifecycle")
    let syncLifecycleGate = SyncLifecycleGate()

    var errorMessage: String?

    /// Foreground refresh poller; picks up iroh-applied writes even if a sync
    /// activity callback was missed. iOS is foreground-sync for this release.
    private var refreshTimer: Timer?
    private var pendingRefreshTask: Task<Void, Never>?
    var pendingEditTask: Task<Void, Never>?
    private var refreshWorkItem: DispatchWorkItem?
    /// Debounced search refresh so per-keystroke typing collapses into a single
    /// note query (the stale-result guard in `refreshNotes` is preserved).
    private var searchRefreshTask: Task<Void, Never>?
    private static let searchRefreshDebounce: Duration = .milliseconds(150)

    var selection: SidebarSelection = .allNotes {
        didSet { refreshNotes() }
    }

    var searchText: String = "" {
        didSet { scheduleDebouncedSearchRefresh() }
    }

    var isViewingTrash: Bool {
        if case .filter(.trash) = selection { return true }
        return false
    }

    var isViewingProject: Bool {
        if case .project = selection { return true }
        return false
    }

    var isViewingTodoFilter: Bool {
        selection == .filter(.todo)
    }

    /// Editor binding for the selected note (source of truth while editing).
    var editorText: String = ""
    var loadedBody = ""
    var loadedVersion: String?
    var loadingNoteID: String?
    var pendingEdit: (noteID: String, text: String, expectedVersion: String)?
    var rejectedEditorDraft: (noteID: String, text: String)?
    var writingNoteID: String?
    static let editDebounce: Duration = .milliseconds(400)

    var isConfirmingEmptyTrash = false
    var projectAwaitingDeletion: String?

    var emptyNotesTitle: String {
        if !searchText.isEmpty { return "No matches for “\(searchText)”" }
        switch selection {
        case .allNotes: return "No notes yet"
        case let .tag(tag): return "No notes tagged #\(tag)"
        case let .project(tag): return "No notes in \(Self.projectName(tag))"
        case let .filter(filter): return filter.emptyTitle
        }
    }

    /// The reserved namespace that makes a tag a project. Single source of
    /// truth (mirrors `TAG_PREFIX` in crates/kiem-core/src/project.rs).
    /// `nonisolated`: read from the off-main `StoreQuery` mapping.
    nonisolated static let projectTagPrefix = "proj/"

    nonisolated static func projectName(_ tag: String) -> String {
        tag.hasPrefix(projectTagPrefix) ? String(tag.dropFirst(projectTagPrefix.count)) : tag
    }

    var selectedNoteIDs: Set<String> = [] {
        didSet {
            let oldSingle = oldValue.count == 1 ? oldValue.first : nil
            if selectedNoteID != oldSingle { loadSelectedNote() }
        }
    }

    var selectedNoteID: String? {
        get { selectedNoteIDs.count == 1 ? selectedNoteIDs.first : nil }
        set { selectedNoteIDs = newValue.map { [$0] } ?? [] }
    }

    var selectedNote: NoteMetadata? {
        notes.first { $0.id == selectedNoteID }
    }

    init(dataDir: URL) throws {
        self.dataDir = dataDir
        try FileManager.default.createDirectory(at: dataDir, withIntermediateDirectories: true)
        store = try KiemStore.open(dataDir: dataDir.path)
        authorDid = (try? store.deviceDid()) ?? "local"
        refresh()
        startSync()
        beginForegroundPolling()
    }

    /// Tear down the poller and stop the mesh. Runs on the main actor.
    func shutDown() {
        refreshTimer?.invalidate()
        refreshTimer = nil
        pendingEditTask?.cancel()
        pendingRefreshTask?.cancel()
        searchRefreshTask?.cancel()
        refreshWorkItem?.cancel()
        stopSync()
    }

    // MARK: Foreground polling (iOS surrogate for AppKit's DB file watcher)

    func beginForegroundPolling() {
        refreshTimer?.invalidate()
        refreshTimer = Timer.scheduledTimer(withTimeInterval: 3.0, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in self?.scheduleDebouncedRefresh() }
        }
    }

    func pauseForegroundPolling() {
        refreshTimer?.invalidate()
        refreshTimer = nil
        pendingRefreshTask?.cancel()
    }

    func scheduleDebouncedRefresh() {
        pendingRefreshTask?.cancel()
        pendingRefreshTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled, let self else { return }
            self.refresh()
            self.reloadEditorIfExternalWriteChangedIt()
        }
    }

    /// Debounced re-query on search-text changes: coalesces per-keystroke typing
    /// into one note query. Deliberately small so results stay immediate, and it
    /// keeps `refreshNotes`' existing stale-result guard (`self.searchText ==
    /// query`) intact — the latest query wins and older results are dropped.
    private func scheduleDebouncedSearchRefresh() {
        searchRefreshTask?.cancel()
        searchRefreshTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: Self.searchRefreshDebounce)
            guard !Task.isCancelled, let self else { return }
            self.refreshNotes()
        }
    }

    func reloadEditorIfExternalWriteChangedIt() {
        guard let id = selectedNoteID, pendingEditTask == nil, writingNoteID == nil,
              loadingNoteID == nil else { return }
        let selection = selection
        perform { try $0.getNote(id: id) } then: { note in
            guard let note, self.selection == selection, self.selectedNoteID == id,
                  self.pendingEditTask == nil, self.loadingNoteID == nil,
                  note.body != self.editorText
            else { return }
            self.pendingEdit = nil
            self.loadedBody = note.body
            self.loadedVersion = note.version
            self.editorText = note.body
        }
    }

    // MARK: CRUD

    func createNote() {
        // A titled placeholder so the new row reads "Untitled" and the editor
        // opens mid-title rather than on an empty `# ` body.
        perform { [authorDid] in try $0.createNote(body: "# Untitled\n", authorDid: authorDid) }
            then: { meta in
                self.refreshSidebar()
                self.refreshNotes { self.selectedNoteID = meta.id }
            }
    }

    func deleteNote(_ id: String) {
        flushPendingEdit()
        perform { try $0.deleteNote(id: id) } then: { _ in self.refresh() }
    }

    func restoreNote(_ id: String) {
        perform { try $0.restoreNote(id: id) } then: { _ in self.refresh() }
    }

    func setPinned(_ id: String, pinned: Bool) {
        perform { try $0.setPinned(id: id, pinned: pinned) } then: { _ in self.refresh() }
    }

    func addTag(_ id: String, tag: String) {
        perform { try $0.addTag(id: id, tag: tag) } then: { _ in self.refresh() }
    }

    func setTodoChecked(noteID: String, index: UInt32, checked: Bool) {
        flushPendingEdit()
        perform { try $0.setTodoChecked(noteId: noteID, index: index, checked: checked) }
            then: { _ in self.refresh() }
    }

    func emptyTrash() {
        flushPendingEdit()
        perform { try $0.purgeDeleted() } then: { _ in self.refresh() }
    }

    // MARK: Refresh

    func refresh() {
        refreshNotes()
        refreshSidebar()
    }

    func refreshNotes(then completion: (@Sendable @MainActor () -> Void)? = nil) {
        let selection = self.selection
        let query = searchText
        perform { store in
            query.isEmpty
                ? try StoreQuery.notes(for: selection, in: store)
                : try StoreQuery.searchResults(matching: query, in: store)
        } then: { listed in
            guard self.selection == selection, self.searchText == query else { return }
            self.notes = listed
            let visible = Set(listed.map(\.id))
            if !self.selectedNoteIDs.isSubset(of: visible) {
                self.selectedNoteIDs.formIntersection(visible)
            }
            self.refreshOpenTodos()
            completion?()
        }
    }

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

    func refreshSidebar() {
        perform { try StoreQuery.sidebarSnapshot(store: $0) } then: { snapshot in
            self.projects = snapshot.projects
            self.tags = snapshot.tags
            self.filterCounts = snapshot.filterCounts
        }
    }

    // MARK: FFI call plumbing (same as macOS)

    let storeQueue = DispatchQueue(label: "org.kiem.store.ios")

    func perform<T: Sendable>(
        _ work: @escaping @Sendable (KiemStore) throws -> T,
        onFailure: (@Sendable @MainActor () -> Void)? = nil,
        then apply: @escaping @Sendable @MainActor (T) -> Void = { _ in }
    ) {
        let store = self.store
        storeQueue.async {
            let result = Result { try work(store) }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    switch result {
                    case let .success(value):
                        apply(value)
                    case let .failure(error):
                        self.errorMessage = "\(error)"
                        onFailure?()
                    }
                }
            }
        }
    }
}
