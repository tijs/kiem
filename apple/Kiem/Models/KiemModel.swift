import AppKit
import Foundation
import KiemKit
import Observation

/// App-wide state over the Rust core. All reads go through `KiemStore`
/// (denormalized SQLite columns — cheap); the editor's text is the only
/// state Swift owns while a note is open.
///
/// The behaviour lives in six extensions — `+Sync`, `+Refresh`, `+Projects`,
/// `+Editor`, `+Transfer`, `+BulkActions` — while every stored property and
/// every computed view of it stays here, because Swift
/// extensions cannot add them and `@Observable` only tracks stored ones. That
/// also means the observable state below is written from those files, so the
/// setters are internal rather than `private(set)`: the views still only ever
/// read them, but the compiler can no longer be the one enforcing it.
@Observable @MainActor
final class KiemModel {
    let store: KiemStore

    /// Note authorship: this device's iroh identity (same id the CLI uses and
    /// peers see on the mesh). Falls back to "local" only if the identity key
    /// is unreadable — in which case sync is broken too and will say so.
    let authorDid: String

    var notes: [NoteMetadata] = []
    /// Tags excluding the `proj/*` namespace (those surface under Projects).
    var tags: [TagCount] = []
    /// Projects, derived from `proj/*` tags with their note counts.
    var projects: [TagCount] = []
    /// Open todos for the selected project (empty unless viewing one).
    var openTodos: [ProjectTodo] = []
    /// Live match counts per smart filter, shown beside its sidebar row.
    var filterCounts: [SmartFilter: Int] = [:]
    /// Ids of peers currently linked for sync (drives the sync-status UI in U13).
    var connectedPeers: [String] = []
    /// Ids of every paired device (reachable or not) — the denominator for
    /// the sync-status indicator.
    var knownPeers: [String] = []
    /// Last sync send/receive timestamp per peer id; used to show a transient
    /// "syncing" state in the Sync settings pane.
    var lastSyncActivity: [String: Date] = [:]
    /// Human-readable name for this device (defaults to the system host name).
    var deviceName: String = ""

    // MARK: Pairing (the Sync settings pane)

    /// This device's shareable ticket, loaded when the Sync settings pane opens.
    var pairingTicket: String?
    /// Whole seconds left on the open pairing window, or nil when closed —
    /// drives the "Ready to pair" countdown.
    var pairingWindowRemaining: Int?
    /// A pending incoming pairing awaiting the user's Allow/Deny.
    var pairingRequest: PairingRequest?

    var errorMessage: String?
    /// Watches `kiem.db`/`kiem.db-wal` for writes from outside our own mutation
    /// calls (an external `kiem` CLI process, or incoming P2P sync). See
    /// `watchStoreForExternalWrites`.
    var dbWatchSources: [DispatchSourceFileSystemObject] = []
    var pendingRefreshTask: Task<Void, Never>?
    private var terminateObserver: NSObjectProtocol?

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

    /// Whether the Todo smart filter is active — there the grouped open-todo
    /// list *is* the view, so the note rows are skipped entirely.
    var isViewingTodoFilter: Bool {
        selection == .filter(.todo)
    }

    /// Drives the shared Empty Trash confirmation (set from the trash list's
    /// button and the sidebar's context menu; the dialog lives in ContentView).
    var isConfirmingEmptyTrash = false

    /// Permanently erase everything in the trash. Purged notes are tombstoned
    /// in the core so a sync exchange can't resurrect them.
    func emptyTrash() {
        // A pending debounce edit to a note that's about to be purged would
        // otherwise flush against a gone id (spurious error alert).
        flushPendingEdit()
        perform { try $0.purgeDeleted() } then: { _ in self.refresh() }
    }

    /// The project tag a "Delete Project…" asked to purge; drives the shared
    /// confirmation dialog in ContentView.
    var projectAwaitingDeletion: String?

    /// Permanently erase a project and every note tagged into it (trashed
    /// ones included), with the same sync-safe tombstoning as Empty Trash.
    func deleteProject(tag: String) {
        flushPendingEdit()
        perform { try $0.purgeTag(tag: tag) } then: { _ in
            if self.selection == .project(tag) {
                self.selection = .allNotes // didSet refreshes the notes
            }
            self.refresh()
        }
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
    /// (mirrors `TAG_PREFIX` in crates/kiem-core/src/project.rs).
    static let projectTagPrefix = "proj/"

    /// Display name for a project tag: `proj/kiem_app` → `kiem_app`.
    static func projectName(_ tag: String) -> String {
        tag.hasPrefix(projectTagPrefix) ? String(tag.dropFirst(projectTagPrefix.count)) : tag
    }

    /// The note list's (multi-)selection. The editor shows a note only when
    /// exactly one is selected; bulk actions (context menu, drag to sidebar)
    /// act on the whole set.
    var selectedNoteIDs: Set<String> = [] {
        didSet {
            let oldSingle = oldValue.count == 1 ? oldValue.first : nil
            if selectedNoteID != oldSingle { loadSelectedNote() }
        }
    }

    /// The single open note — non-nil only when exactly one note is selected.
    /// Setting it replaces the whole selection (the single-select flows:
    /// create, todo-caption tap, restore-after-refresh).
    var selectedNoteID: String? {
        get { selectedNoteIDs.count == 1 ? selectedNoteIDs.first : nil }
        set { selectedNoteIDs = newValue.map { [$0] } ?? [] }
    }

    /// Editor binding for the selected note. While editing, this is the
    /// source of truth; the store mirrors it a debounce interval behind
    /// (each store write is a full CRDT round-trip plus a search reindex,
    /// and mints a permanent Automerge change — too heavy per keystroke).
    var editorText: String = ""

    /// The body last loaded into the editor or flushed to the store. Lets
    /// `editorTextDidChange` distinguish a programmatic load (no write
    /// needed) from a real user edit.
    var loadedBody = ""

    /// The not-yet-persisted edit, captured as (note, text) when scheduled so
    /// a flush always targets the note that was edited, never the current
    /// selection. Flushed after `Self.editDebounce` of typing silence, and
    /// synchronously wherever the write could otherwise be lost or misordered
    /// (note switch, delete, todo toggle, app quit).
    /// The note whose body is being fetched, or nil when the editor is settled.
    /// The load is async now, so it gates two things it used to get for free by
    /// blocking: a stale fetch must not overwrite a newer one, and keystrokes
    /// typed into the not-yet-filled editor must not be written back as the
    /// note's body.
    var loadingNoteID: String?

    var pendingEdit: (noteID: String, text: String)?
    var pendingEditTask: Task<Void, Never>?
    static let editDebounce: Duration = .milliseconds(400)

    init(dataDir: URL) throws {
        store = try KiemStore.open(dataDir: dataDir.path)
        authorDid = (try? store.deviceDid()) ?? "local"
        refresh()
        startSync()
        watchStoreForExternalWrites(dataDir: dataDir)
        // Cmd-Q inside the debounce window must not lose the last keystrokes.
        terminateObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.flushPendingEditBlocking() }
        }
    }

    /// Runs on the main actor (SE-0371 isolated deinit), so the watch sources
    /// and tasks — all main-actor state — can be torn down without
    /// `nonisolated(unsafe)` escape hatches.
    isolated deinit {
        for source in dbWatchSources {
            source.cancel()
        }
        pendingRefreshTask?.cancel()
        pendingEditTask?.cancel()
        if let terminateObserver {
            NotificationCenter.default.removeObserver(terminateObserver)
        }
        store.stopSync()
    }

    // MARK: Import / export (File menu)

    /// A running import/export, for the modal progress sheet. `total` is 0
    /// until the first progress callback arrives (indeterminate).
    struct TransferActivity: Equatable {
        var verb: String
        var done = 0
        var total = 0
    }

    /// Non-nil while an import or export runs — disables the menu items and
    /// presents the progress sheet.
    var activeTransfer: TransferActivity?
    /// Success summary of the last finished transfer (drives its alert).
    var transferMessage: String?
    /// The finished transfer's result, held until the progress sheet is fully
    /// dismissed (see `runTransfer`).
    var pendingTransferOutcome: (message: String, isError: Bool)?

    var selectedNote: NoteMetadata? {
        notes.first { $0.id == selectedNoteID }
    }

    func createNote() {
        perform { [authorDid] in try $0.createNote(body: "# ", authorDid: authorDid) } then: { meta in
            self.refreshSidebar()
            // Select only once the list holds the new note — refreshNotes prunes
            // selections it can't see.
            self.refreshNotes { self.selectedNoteID = meta.id }
        }
    }

    /// Open a note from a `kiem://note/<id>` reference. Trashed notes land in
    /// Trash so the user sees the deleted state; live notes land in All Notes.
    /// Unknown ids surface an error without changing the current selection.
    func openNote(id: String) {
        perform { try $0.getNote(id: id) } then: { note in
            guard let note else {
                self.errorMessage = "No note found for that reference."
                return
            }
            self.selection = note.metadata.deleted ? .filter(.trash) : .allNotes
            self.refreshSidebar()
            self.refreshNotes { self.selectedNoteID = id }
        }
    }

    let storeQueue = DispatchQueue(label: "org.kiem.store")

    /// Run a store call off the main thread, then apply its result on the main
    /// actor. Failures surface in the UI instead of crashing, and skip `apply`
    /// so state is never overwritten from a call that didn't land — pass
    /// `onFailure` where skipping would leave state wedged rather than stale.
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
