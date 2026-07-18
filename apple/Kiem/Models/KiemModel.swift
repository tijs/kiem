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
        report { try store.purgeDeleted() }
        refresh()
    }

    /// The project tag a "Delete Project…" asked to purge; drives the shared
    /// confirmation dialog in ContentView.
    var projectAwaitingDeletion: String?

    /// Permanently erase a project and every note tagged into it (trashed
    /// ones included), with the same sync-safe tombstoning as Empty Trash.
    func deleteProject(tag: String) {
        flushPendingEdit()
        report { try store.purgeTag(tag: tag) }
        if selection == .project(tag) {
            selection = .allNotes
        }
        refresh()
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
            MainActor.assumeIsolated { self?.flushPendingEdit() }
        }
    }

    // Runs on the main actor (SE-0371 isolated deinit), so the watch sources
    // and tasks — all main-actor state — can be torn down without
    // `nonisolated(unsafe)` escape hatches.
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
        // Prune selected notes that left the current view (trashed, filtered
        // out, edited elsewhere) — for a single selection this also clears
        // the editor via the selection didSet.
        let visible = Set(listed.map(\.id))
        if !selectedNoteIDs.isSubset(of: visible) {
            selectedNoteIDs.formIntersection(visible)
        }
        refreshOpenTodos()
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
            openTodos = report { try store.listTodoItemsForTag(tag: tag) } ?? []
        case .filter(.todo):
            openTodos = report { try store.listOpenTodoItems() } ?? []
        default:
            openTodos = []
        }
    }

    /// Toggle a project todo by its (note, index) address and refresh.
    func toggleProjectTodo(noteID: String, index: UInt32, checked: Bool) {
        // A pending body edit to the same note would clobber the toggle.
        flushPendingEdit()
        report { try store.setTodoChecked(noteId: noteID, index: index, checked: checked) }
        refresh()
        // If the toggled note is open in the editor, re-sync its text. Otherwise
        // the editor keeps the pre-toggle body and the next keystroke writes it
        // back, silently reverting the checkbox. loadSelectedNote sets loadedBody
        // before editorText, so the change it triggers sees them equal and skips.
        if noteID == selectedNoteID { loadSelectedNote() }
    }

    /// Rename a project todo by its (note, index) address and refresh.
    /// Same clobber guards as `toggleProjectTodo` (see comments there).
    func updateProjectTodoText(noteID: String, index: UInt32, text: String) {
        flushPendingEdit()
        report { try store.setTodoText(noteId: noteID, index: index, text: text) }
        refresh()
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
        if let counts = report({ try store.filterCounts() }) {
            filterCounts = [
                .todo: Int(counts.todo),
                .today: Int(counts.today),
                .untagged: Int(counts.untagged),
                .pinned: Int(counts.pinned),
                .trash: Int(counts.trash),
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
        report { try store.createNote(body: "# \(name)\n\nProject home.\n\n#\(tag)", authorDid: authorDid) }
        refresh()
        selection = .project(tag)
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

    /// One transfer at a time: run `work` on a background task (it holds the
    /// store+sync lock — sync rounds stall until it finishes, by design),
    /// stream progress into `activeTransfer`, then refresh and report the
    /// summary or error back on the main actor.
    private func runTransfer(
        verb: String,
        work: @escaping @Sendable (TransferProgressRelay) throws -> TransferSummary,
        summary: @escaping @Sendable (TransferSummary) -> String
    ) {
        guard activeTransfer == nil else { return }
        activeTransfer = TransferActivity(verb: verb)
        let relay = TransferProgressRelay { [weak self] done, total in
            Task { @MainActor in
                guard var transfer = self?.activeTransfer else { return }
                transfer.done = done
                transfer.total = total
                self?.activeTransfer = transfer
            }
        }
        Task.detached {
            let result = Result { try work(relay) }
            await MainActor.run { [weak self] in
                guard let self else { return }
                self.activeTransfer = nil
                switch result {
                case let .success(transferred):
                    self.refresh()
                    self.transferMessage = summary(transferred)
                case let .failure(error):
                    self.errorMessage = "\(error)"
                }
            }
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
            try store.createNote(body: "# ", authorDid: authorDid)
        }) else { return }
        refresh()
        selectedNoteID = meta.id
    }

    func deleteNote(id: String) {
        trashNotes([id])
    }

    /// Restore a trashed note (undo a "Move to Trash").
    func restoreNote(id: String) {
        restoreNotes([id])
    }

    // MARK: Bulk actions (multi-select context menu + drag to sidebar)

    /// Move notes to trash. One action for a single note or a selection.
    func trashNotes(_ ids: Set<String>) {
        // Don't let a pending edit land in a note after it's trashed.
        flushPendingEdit()
        for id in ids {
            report { try store.deleteNote(id: id) }
        }
        selectedNoteIDs.subtract(ids)
        refresh()
    }

    /// Restore trashed notes (undo "Move to Trash").
    func restoreNotes(_ ids: Set<String>) {
        for id in ids {
            report { try store.restoreNote(id: id) }
        }
        refresh()
    }

    /// Pin or unpin notes.
    func setPinned(_ ids: Set<String>, pinned: Bool) {
        for id in ids {
            report { try store.setPinned(id: id, pinned: pinned) }
        }
        refresh()
    }

    /// Add a hashtag to notes — both "tag it" (plain tag) and "add to
    /// project" (a `proj/<slug>` tag) are this one operation. Appends to the
    /// body unless already present, so it's the same sync-safe body-update
    /// path as typing the tag; the open note reloads to show it.
    func addTag(_ ids: Set<String>, tag: String) {
        // A pending edit flushed later would overwrite the tagged body.
        flushPendingEdit()
        for id in ids {
            report { try store.addTag(id: id, tag: tag) }
        }
        refresh()
        if let selected = selectedNoteID, ids.contains(selected) {
            loadSelectedNote()
        }
    }

    /// Editor change → schedule a debounced store write (Rust re-derives
    /// title/tags at flush time, then metadata refreshes).
    func editorTextDidChange() {
        guard let id = selectedNoteID else { return }
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

    /// Persist the pending edit now (if any) and refresh derived state.
    private func flushPendingEdit() {
        pendingEditTask?.cancel()
        pendingEditTask = nil
        guard let (id, text) = pendingEdit else { return }
        pendingEdit = nil
        if id == selectedNoteID { loadedBody = text }
        report { try store.updateNote(id: id, body: text) }
        refreshNotes()
        refreshSidebar()
    }

    private func loadSelectedNote() {
        // Selection already points at the new note; this persists the
        // previous note's still-pending text before its editor state goes away.
        flushPendingEdit()
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
    private let onApprove: @Sendable (_ peerId: String) -> Bool

    init(
        onChange: @escaping @Sendable (_ peerId: String, _ connected: Bool) -> Void,
        onApprove: @escaping @Sendable (_ peerId: String) -> Bool
    ) {
        self.onChange = onChange
        self.onApprove = onApprove
    }

    func onConnected(peerId: String) {
        onChange(peerId, true)
    }

    func onDisconnected(peerId: String) {
        onChange(peerId, false)
    }

    func approvePairing(peerId: String) -> Bool {
        onApprove(peerId)
    }
}
