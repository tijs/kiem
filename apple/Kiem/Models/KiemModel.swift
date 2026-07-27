import AppKit
import Foundation
import KiemKit
import Observation

/// App-wide state over the Rust core. All reads go through `KiemStore`
/// (denormalized SQLite columns — cheap); the editor's text is the only
/// state Swift owns while a note is open.
@Observable @MainActor
final class KiemModel {
    private let store: KiemStore

    /// Note authorship: this device's iroh identity (same id the CLI uses and
    /// peers see on the mesh). Falls back to "local" only if the identity key
    /// is unreadable — in which case sync is broken too and will say so.
    private let authorDid: String

    private(set) var notes: [NoteMetadata] = []
    /// Tags excluding the `proj/*` namespace (those surface under Projects).
    private(set) var tags: [TagCount] = []
    /// Projects, derived from `proj/*` tags with their note counts.
    private(set) var projects: [TagCount] = []
    /// Open todos for the selected project (empty unless viewing one).
    private(set) var openTodos: [ProjectTodo] = []
    /// Live match counts per smart filter, shown beside its sidebar row.
    private(set) var filterCounts: [SmartFilter: Int] = [:]
    /// Ids of peers currently linked for sync (drives the sync-status UI in U13).
    private(set) var connectedPeers: [String] = []
    /// Ids of every paired device (reachable or not) — the denominator for
    /// the sync-status indicator.
    private(set) var knownPeers: [String] = []
    /// Last sync send/receive timestamp per peer id; used to show a transient
    /// "syncing" state in the Sync settings pane.
    private(set) var lastSyncActivity: [String: Date] = [:]
    /// Human-readable name for this device (defaults to the system host name).
    private(set) var deviceName: String = ""

    // MARK: Pairing (the Sync settings pane)

    /// This device's shareable ticket, loaded when the Sync settings pane opens.
    private(set) var pairingTicket: String?
    /// Whole seconds left on the open pairing window, or nil when closed —
    /// drives the "Ready to pair" countdown.
    private(set) var pairingWindowRemaining: Int?
    /// A pending incoming pairing awaiting the user's Allow/Deny.
    var pairingRequest: PairingRequest?

    var errorMessage: String?
    /// Watches `kiem.db`/`kiem.db-wal` for writes from outside our own mutation
    /// calls (an external `kiem` CLI process, or incoming P2P sync). See
    /// `watchStoreForExternalWrites`.
    private var dbWatchSources: [DispatchSourceFileSystemObject] = []
    private var pendingRefreshTask: Task<Void, Never>?
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
    private var loadedBody = ""

    /// The not-yet-persisted edit, captured as (note, text) when scheduled so
    /// a flush always targets the note that was edited, never the current
    /// selection. Flushed after `Self.editDebounce` of typing silence, and
    /// synchronously wherever the write could otherwise be lost or misordered
    /// (note switch, delete, todo toggle, app quit).
    private var pendingEdit: (noteID: String, text: String)?
    private var pendingEditTask: Task<Void, Never>?
    private static let editDebounce: Duration = .milliseconds(400)

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

    /// Begin P2P sync: join the iroh mesh in the Rust core (`kiem-sync`), the
    /// same transport and known-peers store the CLI daemon uses. Peers are
    /// added by ticket pairing (`kiem pair`); incoming sync writes land in the
    /// shared SQLite store, where the DB watcher picks them up for the UI.
    private func startSync() {
        knownPeers = (try? store.knownPeers()) ?? []
        deviceName = store.deviceName()
        let events = SyncPeerEvents(
            onChange: { [weak self] peerId, connected in
                Task { @MainActor in
                    guard let self else { return }
                    var peers = Set(self.connectedPeers)
                    if connected { peers.insert(peerId) } else { peers.remove(peerId) }
                    self.connectedPeers = peers.sorted()
                    // A newly-trusted peer connecting is the natural moment to
                    // re-read the roster.
                    self.knownPeers = (try? self.store.knownPeers()) ?? []
                }
            },
            onActivity: { [weak self] peerId in
                Task { @MainActor in
                    guard let self else { return }
                    self.lastSyncActivity[peerId] = Date()
                }
            },
            onApprove: { [weak self] peerId in
                // Called on a Rust blocking thread; block it until the user
                // answers the Allow/Deny prompt we raise on the main actor.
                guard let self else { return false }
                let gate = ApprovalGate()
                Task { @MainActor in
                    self.requestPairingApproval(peerId: peerId, gate: gate)
                }
                return gate.wait()
            }
        )
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

    /// The pairing window's length. Long enough to walk to the other device,
    /// short enough that a leaked code goes stale quickly.
    private static let pairingWindowSecs: UInt64 = 120

    /// Arms (or re-arms) the pairing window and (re)loads this device's ticket
    /// — which briefly waits for a relay hint, so it runs off the main actor.
    /// Called when the Sync settings pane appears.
    func armPairingWindow() {
        store.armPairing(windowSecs: Self.pairingWindowSecs)
        pairingWindowRemaining = Int(Self.pairingWindowSecs)
        let store = self.store
        Task.detached {
            let ticket = try? store.pairTicket()
            await MainActor.run { self.pairingTicket = ticket }
        }
    }

    /// Closes the pairing window (arming for 0s expires it) — called when the
    /// Sync settings pane goes away, so we stop accepting new devices.
    func closePairingWindow() {
        pairingTicket = nil
        pairingWindowRemaining = nil
        store.armPairing(windowSecs: 0)
    }

    /// Re-reads the countdown; the Pair sheet calls this once a second.
    func refreshPairingWindow() {
        pairingWindowRemaining = store.pairingWindowRemaining().map(Int.init)
    }

    /// Trusts and dials the device behind a pasted/scanned ticket.
    func addDevice(ticket: String) {
        let trimmed = ticket.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        let store = self.store
        Task.detached {
            do {
                _ = try store.addKnownPeer(ticket: trimmed)
                await MainActor.run {
                    self.knownPeers = (try? self.store.knownPeers()) ?? []
                }
            } catch {
                await MainActor.run {
                    self.errorMessage = "Couldn't add that device. Check the code and try again."
                }
            }
        }
    }

    /// Unpairs a device — for a machine you no longer have. The Rust core drops
    /// it from the trust list (so it can neither dial in nor be dialed), forgets
    /// its name and sync state, and closes any live session. Notes already
    /// synced from it stay; only the link goes.
    func forgetDevice(peerId: String) {
        let store = self.store
        Task.detached {
            do {
                _ = try store.forgetKnownPeer(peerId: peerId)
                await MainActor.run {
                    self.knownPeers = (try? self.store.knownPeers()) ?? []
                    self.connectedPeers.removeAll { $0 == peerId }
                    self.lastSyncActivity[peerId] = nil
                }
            } catch {
                await MainActor.run {
                    self.errorMessage = "Couldn't unpair that device: \(error)"
                }
            }
        }
    }

    /// Best-known display name for a peer id; falls back to the id itself.
    func peerName(for peerId: String) -> String {
        store.peerName(peerId: peerId)
    }

    /// Rename this device. The new name is sent to peers during the next
    /// handshake and persisted locally.
    func setDeviceName(_ name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        do {
            try store.setDeviceName(name: trimmed)
            deviceName = trimmed
        } catch {
            errorMessage = "Couldn't rename this device: \(error)"
        }
    }

    /// Records an incoming pairing awaiting the user's decision. Only one prompt
    /// shows at a time; a second concurrent request is auto-denied.
    func requestPairingApproval(peerId: String, gate: ApprovalGate) {
        guard pairingRequest == nil else {
            gate.resolve(false)
            return
        }
        pairingRequest = PairingRequest(peerId: peerId, gate: gate)
    }

    /// Answers the pending pairing prompt, unblocking the waiting sync thread.
    func resolvePairing(_ allow: Bool) {
        guard let request = pairingRequest else { return }
        pairingRequest = nil
        request.gate.resolve(allow)
        if allow {
            knownPeers = (try? store.knownPeers()) ?? []
        }
    }

    /// Watch the shared SQLite store for writes from outside our own mutation
    /// calls: an external `kiem` CLI process, or an incoming P2P sync applied by
    /// the Rust mesh (`kiem-sync`). Both land in the same on-disk file (WAL
    /// mode — `crates/kiem-core/src/store/`), so one watcher covers both; no
    /// per-note sync callback is needed. Debounced so a burst of writes
    /// triggers one refresh, not one per write — the app's own writes
    /// harmlessly retrigger a refresh too, which isn't worth special-casing away.
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
            guard !Task.isCancelled, let self, self.activeTransfer == nil else { return }
            self.refresh()
            self.reloadEditorIfExternalWriteChangedIt()
        }
    }

    /// If a sync (or other external) write changed the open note while the
    /// editor was holding stale text, reload the editor so the next flush does
    /// not clobber the incoming change. Active typing is left alone: the pending
    /// edit will merge with the external change on flush.
    private func reloadEditorIfExternalWriteChangedIt() {
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
    private func refreshOpenTodos() {
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

    /// Toggle a project todo by its (note, index) address and refresh.
    func toggleProjectTodo(noteID: String, index: UInt32, checked: Bool) {
        // A pending body edit to the same note would clobber the toggle.
        flushPendingEdit()
        perform { try $0.setTodoChecked(noteId: noteID, index: index, checked: checked) } then: { _ in
            self.refresh()
            // If the toggled note is open in the editor, re-sync its text. Otherwise
            // the editor keeps the pre-toggle body and the next keystroke writes it
            // back, silently reverting the checkbox.
            if noteID == self.selectedNoteID { self.loadSelectedNote() }
        }
    }

    /// Rename a project todo by its (note, index) address and refresh.
    /// Same clobber guards as `toggleProjectTodo` (see comments there).
    func updateProjectTodoText(noteID: String, index: UInt32, text: String) {
        flushPendingEdit()
        perform { try $0.setTodoText(noteId: noteID, index: index, text: text) } then: { _ in
            self.refresh()
            if noteID == self.selectedNoteID { self.loadSelectedNote() }
        }
    }

    /// Full-text search via the Rust core, mapped back to list metadata with
    /// rank order preserved. Trashed hits drop out — they're not in `listNotes`.
    nonisolated private static func searchResults(
        matching query: String, in store: KiemStore
    ) throws -> [NoteMetadata] {
        let hits = try store.search(query: query, limit: 50)
        let byID = try Dictionary(uniqueKeysWithValues: store.listNotes().map { ($0.id, $0) })
        return hits.compactMap { byID[$0.noteId] }
    }

    /// The note list backing a sidebar selection. Each case maps to a dedicated
    /// `KiemStore` query; the filtering itself lives in the Rust core.
    nonisolated private static func notes(
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

    /// Refresh the sidebar's tag list, project list, and smart-filter counts.
    /// One store call for both, so the counts can't disagree with the tags.
    private func refreshSidebar() {
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
                .trash: Int(result.counts.trash),
            ]
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
        let body = "# \(name)\n\nProject home.\n\n#\(tag)"
        perform { [authorDid] in try $0.createNote(body: body, authorDid: authorDid) } then: { _ in
            self.refreshSidebar()
            self.selection = .project(tag) // didSet refreshes the notes
        }
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
    private(set) var activeTransfer: TransferActivity?
    /// Success summary of the last finished transfer (drives its alert).
    var transferMessage: String?

    /// Export every project's notes as Markdown files under `dir` (one folder
    /// per project), off the main actor with live progress.
    func exportNotes(to dir: URL) {
        runTransfer(verb: "Exporting", work: { [store] relay in
            try store.exportNotes(dir: dir.path, progress: relay)
        }, summary: { summary in
            var text = "Exported \(summary.transferred) notes to “\(dir.lastPathComponent)”."
            if summary.skipped > 0 {
                text += " Skipped \(summary.skipped) notes that aren’t in a project."
            }
            return text
        })
    }

    /// Import a folder of Markdown files as notes, off the main actor with
    /// live progress. `foldersAsProjects` maps folders to projects (subfolders
    /// each, or the flat folder itself); without it notes keep only the tags
    /// already in their bodies (e.g. a Bear/Obsidian dump).
    func importNotes(from dir: URL, foldersAsProjects: Bool) {
        runTransfer(verb: "Importing", work: { [store, authorDid] relay in
            try store.importNotes(
                dir: dir.path,
                authorDid: authorDid,
                foldersAsProjects: foldersAsProjects,
                progress: relay
            )
        }, summary: { summary in
            var text = "Imported \(summary.transferred) notes from “\(dir.lastPathComponent)”."
            if summary.skipped > 0 {
                text += " \(summary.skipped) were already present and were skipped."
            }
            return text
        })
    }

    /// One transfer at a time: run `work` on the store queue (it holds the
    /// store+sync lock — sync rounds stall until it finishes, by design),
    /// stream progress into `activeTransfer`, then refresh and report the
    /// summary or error back on the main actor.
    private func runTransfer(
        verb: String,
        work: @escaping @Sendable (TransferProgressRelay) throws -> TransferSummary,
        summary: @escaping @Sendable (TransferSummary) -> String
    ) {
        guard activeTransfer == nil else { return }
        // The transfer holds the store lock for its whole run. Flushing first
        // puts the pending write ahead of it on the store queue — the modal
        // progress sheet prevents new edits until the transfer ends.
        flushPendingEdit()
        activeTransfer = TransferActivity(verb: verb)
        let relay = TransferProgressRelay { [weak self] done, total in
            Task { @MainActor in
                guard var transfer = self?.activeTransfer else { return }
                // Independent task hops aren't FIFO — drop a late, smaller
                // update instead of walking the bar backwards.
                guard done > transfer.done || total != transfer.total else { return }
                transfer.done = done
                transfer.total = total
                self?.activeTransfer = transfer
            }
        }
        storeQueue.async {
            let result = Result { try work(relay) }
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    switch result {
                    case let .success(transferred):
                        self.refresh()
                        self.pendingTransferOutcome = (summary(transferred), isError: false)
                    case let .failure(error):
                        self.pendingTransferOutcome = ("\(error)", isError: true)
                    }
                    // Dropping activeTransfer dismisses the progress sheet; the
                    // outcome alert waits for the sheet's onDismiss — presenting
                    // it while the sheet is still tearing down can silently drop
                    // it on macOS.
                    self.activeTransfer = nil
                }
            }
        }
    }

    /// The finished transfer's result, held until the progress sheet is fully
    /// dismissed (see `runTransfer`).
    private var pendingTransferOutcome: (message: String, isError: Bool)?

    /// Called from the progress sheet's `onDismiss`: surface the held outcome.
    func transferSheetDismissed() {
        guard let (message, isError) = pendingTransferOutcome else { return }
        pendingTransferOutcome = nil
        if isError {
            errorMessage = message
        } else {
            transferMessage = message
        }
    }

    /// `proj/<slug>` from a free-form name. Byte-for-byte mirror of the Rust
    /// `to_tag`/`slugify` in `crates/kiem-core/src/project.rs`, enforced by the
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
            let out: Character
            if let byte = ch.asciiValue, (65 ... 90).contains(byte) {
                out = Character(UnicodeScalar(byte + 32))
            } else {
                out = ch
            }
            if let byte = out.asciiValue, (97 ... 122).contains(byte) || (48 ... 57).contains(byte) || out == "/" {
                slug.append(out)
                prevSep = false
            } else if out == " " || out == "-" || out == "_" {
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

    // MARK: Bulk actions (multi-select context menu + drag to sidebar)

    /// Move notes to trash. One action for a single note or a selection.
    func trashNotes(_ ids: Set<String>) {
        // Don't let a pending edit land in a note after it's trashed.
        flushPendingEdit()
        let replacementID: String?
        if ids.count == 1,
           let selected = selectedNoteID,
           ids.contains(selected),
           let index = notes.firstIndex(where: { $0.id == selected })
        {
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

    /// Editor change → schedule a debounced store write (Rust re-derives
    /// title/tags at flush time, then metadata refreshes).
    func editorTextDidChange() {
        guard let id = selectedNoteID, loadingNoteID == nil else { return }
        // Loading a note assigns `editorText` programmatically, which also fires
        // this handler. Skip when nothing actually changed: otherwise every
        // note-open re-derives + persists metadata (bumping modified_at and, with
        // a mismatched embedded core, clobbering data). See
        // docs/solutions/integration-issues/stale-prebuilt-kiemkit-xcframework-clobbers-tags-2026-06-20.md
        guard editorText != loadedBody else { return }
        pendingEdit = (noteID: id, text: editorText)
        pendingEditTask?.cancel()
        pendingEditTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: Self.editDebounce)
            guard !Task.isCancelled else { return }
            self?.flushPendingEdit()
        }
    }

    /// Take the pending edit, if any, and stop its debounce timer.
    private func takePendingEdit() -> (noteID: String, text: String)? {
        pendingEditTask?.cancel()
        pendingEditTask = nil
        guard let edit = pendingEdit else { return nil }
        pendingEdit = nil
        if edit.noteID == selectedNoteID { loadedBody = edit.text }
        return edit
    }

    /// Queue the pending edit's write (if any) and refresh derived state. The
    /// write is *enqueued* synchronously, so anything queued after it — a tag
    /// add, a delete, the next note's load — still lands after it.
    private func flushPendingEdit() {
        guard let (id, text) = takePendingEdit() else { return }
        perform { try $0.updateNote(id: id, body: text) } then: { _ in
            self.refreshNotes()
            self.refreshSidebar()
        }
    }

    /// Persist the pending edit *before returning*. Only for app termination,
    /// where an enqueued write would die with the process. Blocks the main
    /// thread by design — at quit that's a beat, not a beachball.
    private func flushPendingEditBlocking() {
        guard let (id, text) = takePendingEdit() else { return }
        let store = self.store
        storeQueue.sync { try? store.updateNote(id: id, body: text) }
    }

    /// The note whose body is being fetched, or nil when the editor is settled.
    /// The load is async now, so it gates two things it used to get for free by
    /// blocking: a stale fetch must not overwrite a newer one, and keystrokes
    /// typed into the not-yet-filled editor must not be written back as the
    /// note's body.
    private var loadingNoteID: String?

    private func loadSelectedNote() {
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
    private let storeQueue = DispatchQueue(label: "org.kiem.store")

    /// Run a store call off the main thread, then apply its result on the main
    /// actor. Failures surface in the UI instead of crashing, and skip `apply`
    /// so state is never overwritten from a call that didn't land — pass
    /// `onFailure` where skipping would leave state wedged rather than stale.
    private func perform<T: Sendable>(
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

/// Forwards Rust transfer progress (delivered on the transfer's background
/// thread) to the main actor. Holds only a @Sendable closure.
final class TransferProgressRelay: TransferProgress, @unchecked Sendable {
    private let update: @Sendable (Int, Int) -> Void

    init(update: @escaping @Sendable (Int, Int) -> Void) {
        self.update = update
    }

    func onProgress(done: UInt32, total: UInt32) {
        update(Int(done), Int(total))
    }
}

/// A pending incoming pairing shown to the user as Allow/Deny. Holds the
/// `ApprovalGate` the sync thread is blocked on until they decide.
struct PairingRequest: Identifiable {
    let id = UUID()
    let peerId: String
    let gate: ApprovalGate

    /// A short, human-glanceable form of the device id for the prompt.
    var shortPeerId: String {
        String(peerId.prefix(12))
    }
}

/// Blocks the Rust `approve_pairing` thread until the UI resolves it. The
/// semaphore provides the happens-before edge, so the unsynchronized `result`
/// is safe to write-then-signal / wait-then-read.
final class ApprovalGate: @unchecked Sendable {
    private let semaphore = DispatchSemaphore(value: 0)
    private var result = false

    func resolve(_ allow: Bool) {
        result = allow
        semaphore.signal()
    }

    func wait() -> Bool {
        semaphore.wait()
        return result
    }
}

/// Bridges `kiem-sync` mesh callbacks (arriving on Rust threads) to Sendable
/// closures; the model hops them onto the main actor. `onApprove` runs on a
/// blocking sync thread and returns the user's decision (see `ApprovalGate`).
private final class SyncPeerEvents: PeerEvents {
    private let onChange: @Sendable (_ peerId: String, _ connected: Bool) -> Void
    private let onActivity: @Sendable (_ peerId: String) -> Void
    private let onApprove: @Sendable (_ peerId: String) -> Bool

    init(
        onChange: @escaping @Sendable (_ peerId: String, _ connected: Bool) -> Void,
        onActivity: @escaping @Sendable (_ peerId: String) -> Void,
        onApprove: @escaping @Sendable (_ peerId: String) -> Bool
    ) {
        self.onChange = onChange
        self.onActivity = onActivity
        self.onApprove = onApprove
    }

    func onConnected(peerId: String) {
        onChange(peerId, true)
    }

    func onDisconnected(peerId: String) {
        onChange(peerId, false)
    }

    func onSyncActivity(peerId: String) {
        onActivity(peerId)
    }

    func approvePairing(peerId: String) -> Bool {
        onApprove(peerId)
    }
}
