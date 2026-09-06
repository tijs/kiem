import Foundation
import KiemKit
import Testing
@testable import Kiem_iOS

/// Thread-safe scratch store helper: a uniquely-named temp directory, opened
/// through the real `KiemStore`/Rust core. Returns a handle you keep for the
/// test's lifetime.
@discardableResult
func makeScratchStore() throws -> (KiemStore, URL) {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("kiem-ios-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    let store = try KiemStore.open(dataDir: dir.path)
    return (store, dir)
}

func makeFixture(
    in store: KiemStore,
    body: String,
    pinned: Bool = false,
    delete: Bool = false,
    authorDid: String? = nil
) throws -> NoteMetadata {
    let author = try authorDid ?? (try store.deviceDid())
    var meta = try store.createNote(body: body, authorDid: author)
    if pinned { meta = try store.setPinned(id: meta.id, pinned: true) }
    if delete { meta = try store.deleteNote(id: meta.id) }
    return meta
}

/// A few seconds of main-runloop time so `KiemModel.perform` (storeQueue →
/// main hop) and debounce timers can land. Only used from @MainActor tests.
@MainActor
func pumpMain(ms: Int) async {
    try? await Task.sleep(for: .milliseconds(ms))
}

@Suite("Store query / grouping mapping exercises the real Rust core")
struct StoreQueryTests {
    @Test func smartFiltersAndTagsMapToCorrectNotes() throws {
        let (store, _) = try makeScratchStore()
        let planBody = "# Plan Alpha\n- [ ] do it\n- [ ] also\n#tag1\n#proj/alpha"
        let plan = try makeFixture(in: store, body: planBody)
        let plainBody = "# Plain\n#tag1"
        let plain = try makeFixture(in: store, body: plainBody)
        _ = try makeFixture(in: store, body: "# No Labels\njust words")
        _ = try makeFixture(in: store, body: "# Pinned One\n#proj/alpha", pinned: true)
        _ = try makeFixture(in: store, body: "# Trash Me\n#tag2", delete: true)

        // All Notes excludes trashed notes.
        let all = try StoreQuery.notes(for: .allNotes, in: store)
        #expect(all.count == 4)
        #expect(all.contains { $0.id == plan.id })
        #expect(all.contains { $0.id == plain.id })

        // Filters map to their dedicated queries.
        let todo = try StoreQuery.notes(for: .filter(.todo), in: store)
        #expect(todo.map(\.id) == [plan.id])
        let pinned = try StoreQuery.notes(for: .filter(.pinned), in: store)
        #expect(pinned.allSatisfy { $0.pinned })
        #expect(pinned.count == 1)
        let untagged = try StoreQuery.notes(for: .filter(.untagged), in: store)
        #expect(untagged.allSatisfy { $0.tags.isEmpty })
        let trash = try StoreQuery.notes(for: .filter(.trash), in: store)
        #expect(trash.allSatisfy { $0.deleted })

        // Tags and projects (reserved proj/* prefix) map to listByTag.
        let tag1 = try StoreQuery.notes(for: .tag("tag1"), in: store)
        #expect(Set(tag1.map(\.id)) == Set([plan.id, plain.id]))
        let proj = try StoreQuery.notes(for: .project("proj/alpha"), in: store)
        #expect(Set(proj.map(\.id)).count == 2)

        // Sidebar snapshot splits projects vs plain tags, and reports counts.
        let snapshot = try StoreQuery.sidebarSnapshot(store: store)
        #expect(snapshot.projects.map(\.tag) == ["proj/alpha"])
        #expect(snapshot.tags.map(\.tag) == ["tag1"])
        #expect(snapshot.filterCounts[.todo] == 1)
        #expect(snapshot.filterCounts[.pinned] == 1)
        #expect(snapshot.filterCounts[.untagged] == 1)
        #expect(snapshot.filterCounts[.trash] == 1)

        // Content-derivation parity: the Pulp analyzer agrees with what the
        // Rust core re-derives for title/tags.
        #expect(KiemModel.derive(titleFrom: planBody) == "Plan Alpha")
        #expect(KiemModel.derive(tagsFrom: planBody).contains("tag1"))
        #expect(KiemModel.derive(hasUncheckedTodosFrom: planBody))
    }
}

@Suite("Version-aware writes reject stale whole-body edits")
struct VersionConflictTests {
    @Test func staleWriterGetsConflictAndLatestWins() throws {
        let (store, _) = try makeScratchStore()
        let author = try store.deviceDid()

        // Read the note to capture its version token.
        let meta = try store.createNote(body: "# Original\n-v1", authorDid: author)
        let read = try #require(try store.getNote(id: meta.id))
        let expectedVersion = read.version

        // Another writer wins the race (a synced peer, for instance).
        _ = try store.updateNote(id: meta.id, body: "# Newer\n-v2")

        // Our version-checked stale write must be rejected with Conflict…
        let latest = try store.getNote(id: meta.id)
        #expect(latest?.body == "# Newer\n-v2")

        // … and the rejected body is not silently applied.
        do {
            _ = try store.updateNoteIfVersion(id: meta.id, body: "# Stale\noverwrite", expectedVersion: expectedVersion)
            Issue.record("stale version-checked write should have been rejected")
        } catch {
            // Conflict surfaces as an error; the store keeps the newer body.
            let after = try store.getNote(id: meta.id)
            #expect(after?.body == "# Newer\n-v2")
        }
    }

    @Test func unrelatedNoteRotationDoesNotInvalidateOurVersion() throws {
        let (store, _) = try makeScratchStore()
        let author = try store.deviceDid()
        let a = try store.createNote(body: "# A", authorDid: author)
        let b = try store.createNote(body: "# B", authorDid: author)
        let readA = try #require(try store.getNote(id: a.id))
        // B rotates (as if edited/synced elsewhere); A is untouched.
        _ = try store.updateNote(id: b.id, body: "# B2")
        // A's version still validates against the store.
        let ok = try store.updateNoteIfVersion(id: a.id, body: "# A2", expectedVersion: readA.version)
        #expect(ok.body == "# A2")
        #expect(try store.listNotes().count == 2)
    }
}

@Suite("Adaptive navigation policy")
struct NavigationPolicyTests {
    @Test func compactStacksAndRegularKeepsSidebar() {
        #expect(KiemNavigationPolicy.policy(for: .compact) == .stackedDetail)
        #expect(KiemNavigationPolicy.policy(for: .regular) == .sidebarAndDetail)
        #expect(!KiemNavigationPolicy.showsSidebar(for: .compact))
        #expect(KiemNavigationPolicy.showsSidebar(for: .regular))
    }
}

@MainActor
@Suite("Model lifecycle through a temporary sandbox")
struct ModelLifecycleTests {
    @Test func createSelectEditPersistsAcrossReload() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("kiem-ios-lifecycle-\(UUID().uuidString)", isDirectory: true)
        let model = try KiemModel(dataDir: dir)
        defer { model.shutDown() }

        let initialCount = model.notes.count
        model.selection = .allNotes
        await pumpMain(ms: 250)

        // Create; the new note appears in All Notes and is selected.
        model.createNote()
        await pumpMain(ms: 600)
        #expect(model.notes.count == initialCount + 1)
        let noteID = try #require(model.selectedNoteID)
        #expect(model.notes.contains { $0.id == noteID })

        // Opening the note loads its body into the editor buffer.
        model.selectedNoteID = noteID
        await pumpMain(ms: 400)
        #expect(model.editorText.hasPrefix("#"))

        // Edit the Markdown body; the debounced version-aware flush persists it
        // through the Rust store.
        model.editorText = "# Edited on iOS\n- [ ] new task"
        model.editorTextDidChange()
        await pumpMain(ms: 900)
        model.flushPendingEdit()
        await pumpMain(ms: 500)

        // Relaunch against the same directory: the edit survived.
        model.shutDown()
        let reloaded = try KiemModel(dataDir: dir)
        defer { reloaded.shutDown() }
        await pumpMain(ms: 250)
        let note = try #require(try reloaded.store.getNote(id: noteID))
        #expect(note.body == "# Edited on iOS\n- [ ] new task")
        #expect(note.metadata.title == "Edited on iOS")
    }
}

@MainActor
@Suite("Sync mesh lifecycle is idempotent and restarts on foreground return")
struct SyncLifecycleTests {
    @Test func startStopRestartCycleIsIdempotent() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("kiem-ios-sync-\(UUID().uuidString)", isDirectory: true)
        let model = try KiemModel(dataDir: dir)
        defer { model.shutDown() }

        // init() arms the mesh.
        #expect(model.isSyncRunning)

        // Simulate background: the scene stops the mesh.
        model.stopSync()
        #expect(!model.isSyncRunning)

        // Simulate return-to-active: restarting re-arms it.
        model.startSync()
        #expect(model.isSyncRunning)

        // Repeated start while already armed is a no-op (idempotent).
        model.startSync()
        #expect(model.isSyncRunning)

        // Repeated stop while already stopped is a no-op.
        model.stopSync()
        model.stopSync()
        #expect(!model.isSyncRunning)

        // A full stop/re-arm cycle works again (the regression the scene-phase
        // handler depends on: re-start after a background pause).
        model.startSync()
        #expect(model.isSyncRunning)
    }
}

@MainActor
@Suite("Device rename contract: exits edit mode only on success")
struct DeviceRenameContractTests {
    @Test func renameSucceedsAndUpdatesDeviceName() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("kiem-ios-rename-\(UUID().uuidString)", isDirectory: true)
        let model = try KiemModel(dataDir: dir)
        defer { model.shutDown() }

        let ok = model.setDeviceName("Tijs iPhone")
        #expect(ok, "a valid rename should return true so the UI can close the edit field")
        #expect(model.deviceName == "Tijs iPhone")
        #expect(model.errorMessage == nil)
    }

    @Test func blankNameIsRejectedWithoutTouch() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("kiem-ios-rename-\(UUID().uuidString)", isDirectory: true)
        let model = try KiemModel(dataDir: dir)
        defer { model.shutDown() }

        // A fresh store has a default device name; a blank rename must be
        // rejected without touching it or raising an error.
        let before = model.deviceName
        #expect(!before.isEmpty, "scratch store should have a default device name")
        #expect(!model.setDeviceName("   "), "whitespace-only name should report failure")
        #expect(model.deviceName == before, "rejected rename must not change the device name")
        #expect(model.errorMessage == nil)
    }
}

@Suite("Bounded pairing approval gate")
struct ApprovalGateTests {
    @Test func waitReturnsDenyInBoundedTimeWhenUnanswered() {
        let gate = ApprovalGate(timeout: 0.05)
        let start = Date()
        #expect(gate.wait() == false)
        let elapsed = Date().timeIntervalSince(start)
        #expect(elapsed < 5.0, "wait() must be bounded, not block the sync thread forever")
    }

    @Test func resolvedDecisionIsReturnedAndLateOverrideIgnored() {
        let gate = ApprovalGate(timeout: 0.05)
        gate.resolve(true)
        #expect(gate.wait() == true)
        // A late denial (timeout racing the user's approve) is ignored.
        gate.resolve(false)
        #expect(gate.wait() == true)
    }
}

@MainActor
@Suite("Pairing request resolution (Allow/Deny/cancel) drives the gate")
struct PairingApprovalTests {
    @Test func denyDeniesAllowsAllowsAndConcurrentRequestIsAutoDenied() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("kiem-ios-pairing-\\(UUID().uuidString)", isDirectory: true)
        let model = try KiemModel(dataDir: dir)
        defer { model.shutDown() }

        // Deny resolves the blocked sync gate to false (the default when the
        // alert is dismissed without choosing).
        let denyGate = ApprovalGate()
        model.requestPairingApproval(peerId: "peer-abc", gate: denyGate)
        #expect(model.pairingRequest != nil)
        model.resolvePairing(false)
        #expect(model.pairingRequest == nil)
        #expect(denyGate.wait() == false)

        // Allow resolves the gate to true and refreshes known peers.
        let allowGate = ApprovalGate()
        model.requestPairingApproval(peerId: "peer-def", gate: allowGate)
        #expect(model.pairingRequest != nil)
        model.resolvePairing(true)
        #expect(model.pairingRequest == nil)
        #expect(allowGate.wait() == true)

        // A second incoming request while one is pending is auto-denied and
        // the original prompt stays up (no orphaned gates).
        model.requestPairingApproval(peerId: "peer-one", gate: ApprovalGate())
        let secondGate = ApprovalGate()
        model.requestPairingApproval(peerId: "peer-two", gate: secondGate)
        #expect(secondGate.wait() == false)
        #expect(model.pairingRequest?.peerId == "peer-one")
    }
}

@Suite("Pairing countdown mm:ss formatting (shared with the Mac sheet)")
struct PairingCountdownTests {
    @Test func mmssFormatsMinutesAndSeconds() {
        #expect(KiemModel.mmss(0) == "0:00")
        #expect(KiemModel.mmss(59) == "0:59")
        #expect(KiemModel.mmss(60) == "1:00")
        #expect(KiemModel.mmss(90) == "1:30")
        #expect(KiemModel.mmss(119) == "1:59")
        #expect(KiemModel.mmss(120) == "2:00")
    }
}

@Suite("Pairing window activation (ticket may only be shown while discoverable)")
struct PairingWindowActivationTests {
    @Test func unsetOrExpiredWindowIsInactive() {
        // Never armed (nil) and fully elapsed (0) windows must not leave a
        // code presentable.
        #expect(!KiemModel.pairingWindowIsActive(remaining: nil))
        #expect(!KiemModel.pairingWindowIsActive(remaining: 0))
        // A negative/errored value from the store is equally non-discoverable.
        #expect(!KiemModel.pairingWindowIsActive(remaining: -1))
    }

    @Test func activeWindowRemainsDiscoverable() {
        #expect(KiemModel.pairingWindowIsActive(remaining: 1))
        #expect(KiemModel.pairingWindowIsActive(remaining: 120))
    }
}

@Suite("Peer status derivation (Offline / Syncing / Connected)")
struct PeerStatusTests {
    @Test func disconnectedIsAlwaysOfflineEvenIfRecentlyActive() {
        let now = Date()
        #expect(KiemModel.peerStatus(isConnected: false, lastActivity: now.addingTimeInterval(-1), now: now, syncingTimeout: 2) == .offline)
    }

    @Test func connectedWithNoRecentActivityIsConnected() {
        let now = Date()
        #expect(KiemModel.peerStatus(isConnected: true, lastActivity: now.addingTimeInterval(-10), now: now, syncingTimeout: 2) == .connected)
    }

    @Test func connectedWithActivityInsideWindowIsSyncing() {
        let now = Date()
        #expect(KiemModel.peerStatus(isConnected: true, lastActivity: now.addingTimeInterval(-1), now: now, syncingTimeout: 2) == .syncing)
    }

    @Test func activityExactlyAtBoundaryIsConnectedNotSyncing() {
        let now = Date()
        #expect(KiemModel.peerStatus(isConnected: true, lastActivity: now.addingTimeInterval(-2), now: now, syncingTimeout: 2) == .connected)
    }

    @Test func connectedWithNoRecordedActivityIsConnected() {
        let now = Date()
        #expect(KiemModel.peerStatus(isConnected: true, lastActivity: nil, now: now, syncingTimeout: 2) == .connected)
    }
}

@MainActor
@Suite("Instance peerStatus(for:now:) re-renders Syncing → Connected as the clock advances")
struct PeerStatusInstanceTests {
    @Test func connectedPeerRelaxesFromSyncingToConnected() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("kiem-ios-peerstatus-\(UUID().uuidString)", isDirectory: true)
        let model = try KiemModel(dataDir: dir)
        defer { model.shutDown() }

        let peer = "peer-status-test"
        model.connectedPeers = [peer]
        let lastActivity = Date().addingTimeInterval(-1) // inside the 2 s window
        model.lastSyncActivity[peer] = lastActivity

        // At the earlier second the peer shows Syncing; once the view's `now`
        // has advanced past the activity timeout it relaxes to Connected —
        // exactly the transition the observable `@State now` drives each tick.
        let now = Date()
        #expect(model.peerStatus(for: peer, now: now) == .syncing)
        #expect(model.peerStatus(for: peer, now: now.addingTimeInterval(KiemModel.peerSyncingTimeout)) == .connected)
        #expect(model.peerStatus(for: peer, now: now.addingTimeInterval(-10)) == .syncing)
    }
}

@Suite("Sync lifecycle start/stop race gate")
struct SyncLifecycleGateTests {
    @Test func pendingStartIsCancelledBySupersedingStop() {
        let gate = SyncLifecycleGate()
        let gen = gate.requestStart()
        gate.requestStop() // scene backgrounds while the start is still queued
        #expect(!gate.isCurrentStart(gen))
    }

    @Test func unsupersededStartRemainsCurrent() {
        let gate = SyncLifecycleGate()
        let gen = gate.requestStart()
        #expect(gate.isCurrentStart(gen))
        #expect(gate.shouldRevert(gen))
    }

    @Test func staleStartFailureDoesNotUnArmNewerStart() {
        let gate = SyncLifecycleGate()
        let oldGen = gate.requestStart()
        let newGen = gate.requestStart() // newer start supersedes the old one
        #expect(gate.isCurrentStart(newGen))
        #expect(!gate.isCurrentStart(oldGen))
        #expect(!gate.shouldRevert(oldGen))
    }
}

@MainActor
@Suite("Pairing window survives async mesh startup and never resurrects a closed code")
struct PairingWindowStartupTests {
    private func makeModel() throws -> KiemModel {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("kiem-ios-pairwin-\(UUID().uuidString)", isDirectory: true)
        return try KiemModel(dataDir: dir)
    }

    /// Wait (bounded) for the detached ticket fetch to land on the main actor so
    /// the assertion below isn't racing the relay-registration wait.
    @MainActor
    private func waitForTicket(in model: KiemModel, attempts: Int = 60) async {
        for _ in 0..<attempts where model.pairingTicket == nil {
            await pumpMain(ms: 100)
        }
    }

    @Test func armThenRefreshKeepsWindowPresentableWhileMeshStarts() async throws {
        let model = try makeModel()
        defer { model.shutDown() }

        // onAppear arms the window, potentially before the async mesh start has
        // completed — the regression that left the sheet forever unpresentable.
        model.armPairingWindow()
        await waitForTicket(in: model)
        #expect(model.pairingTicket != nil, "a pairing code should become available")

        // The 1 Hz refresh tick used to clear an unmatched window as nil from a
        // not-yet-started mesh; it must keep the window presentable instead.
        model.refreshPairingWindow()
        #expect(model.pairingTicket != nil, "startup refresh must not clear the code")
        #expect(model.pairingWindowIsActive, "the sheet should be discoverable after the startup refresh")
    }

    @Test func closedWindowIsNeverResurrectedByARefresh() async throws {
        let model = try makeModel()
        defer { model.shutDown() }

        model.armPairingWindow()
        model.closePairingWindow()
        #expect(model.pairingTicket == nil, "closing the sheet must drop any code")
        #expect(!model.pairingWindowIsActive)

        // Even a residue ticket lingering in state must be cleared by the next
        // tick once the window is not wanted — an expired/closed code can never
        // remain scannable or shareable.
        model.pairingTicket = "stale-residue"
        model.refreshPairingWindow()
        #expect(model.pairingTicket == nil, "a closed window must not keep a shareable code")
        #expect(!model.pairingWindowIsActive)
    }

    @Test func rearmingAfterCloseRefreshesWindow() async throws {
        let model = try makeModel()
        defer { model.shutDown() }

        model.armPairingWindow()
        model.closePairingWindow()
        // Reopen: a fresh arm must supersede the closed one and present a code,
        // rather than letting the closed window's state linger.
        model.armPairingWindow()
        await waitForTicket(in: model)
        model.refreshPairingWindow()
        #expect(model.pairingTicket != nil, "a reopened sheet should present a fresh code")
        #expect(model.pairingWindowIsActive, "a reopened sheet should be discoverable again")
    }
}
