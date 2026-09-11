import Foundation
import UIKit
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
        // A realistic cross-device handoff needs a five-minute window.
        #expect(KiemModel.mmss(299) == "4:59")
        #expect(KiemModel.mmss(300) == "5:00")
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
        #expect(KiemModel.pairingWindowIsActive(remaining: 300))
    }

    @Test func pairingWindowArmsForFiveMinutes() {
        // Long enough that a user can copy the code, background Kiem, paste it
        // on another device, and still be found — the regression that a two-
        // minute window was too short to survive.
        #expect(KiemModel.pairingWindowSecs == 300)
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

@Suite("Sync overview derivation (status screen summary)")
struct SyncOverviewTests {
    @Test func meshNotRunningIsOfflineWithNoSyncingRegardlessOfPeers() {
        let now = Date()
        let o = SyncOverview.derive(
            meshRunning: false,
            knownPeers: ["a"],
            connectedPeers: ["a"],
            lastActivityByPeer: ["a": now],
            now: now,
            syncingTimeout: 2
        )
        #expect(o.state == .offline)
        #expect(!o.meshRunning)
        #expect(o.syncingCount == 0, "an offline mesh is never 'syncing'")
    }

    @Test func runningWithRecentActivityIsSyncing() {
        let now = Date()
        let o = SyncOverview.derive(
            meshRunning: true,
            knownPeers: ["a", "b"],
            connectedPeers: ["a"],
            lastActivityByPeer: ["a": now.addingTimeInterval(-1)],
            now: now,
            syncingTimeout: 2
        )
        #expect(o.state == .syncing)
        #expect(o.syncingCount == 1)
        #expect(o.knownCount == 2)
        #expect(o.connectedCount == 1)
    }

    @Test func runningWithoutRecentActivityIsIdle() {
        let now = Date()
        let o = SyncOverview.derive(
            meshRunning: true,
            knownPeers: ["a"],
            connectedPeers: ["a"],
            lastActivityByPeer: ["a": now.addingTimeInterval(-10)],
            now: now,
            syncingTimeout: 2
        )
        #expect(o.state == .idle)
        #expect(o.syncingCount == 0)
    }

    @Test func runningWithNoPeersOrActivityIsIdleWithNoLastSync() {
        let now = Date()
        let o = SyncOverview.derive(
            meshRunning: true,
            knownPeers: [],
            connectedPeers: [],
            lastActivityByPeer: [:],
            now: now,
            syncingTimeout: 2
        )
        #expect(o.state == .idle)
        #expect(o.connectedCount == 0)
        #expect(o.knownCount == 0)
        #expect(o.mostRecentActivity == nil)
    }

    @Test func onlyConnectedPeersInsideTimeoutCountTowardSyncing() {
        let now = Date()
        let o = SyncOverview.derive(
            meshRunning: true,
            knownPeers: ["a", "b"],
            connectedPeers: ["a", "b"],
            lastActivityByPeer: [
                "a": now.addingTimeInterval(-1),  // inside 2 s window → syncing
                "b": now.addingTimeInterval(-10), // idle
            ],
            now: now,
            syncingTimeout: 2
        )
        #expect(o.syncingCount == 1)
        #expect(o.state == .syncing)
    }

    @Test func mostRecentActivityTracksLatestEvenForDroppedPeers() {
        let now = Date()
        let recent = now.addingTimeInterval(-1)
        let older = now.addingTimeInterval(-5)
        let o = SyncOverview.derive(
            meshRunning: true,
            knownPeers: ["a", "b", "c"],
            connectedPeers: ["a"],
            lastActivityByPeer: ["a": older, "b": recent],
            now: now,
            syncingTimeout: 2
        )
        // Only the *connected* peer factors into syncing; peer "b" is known but
        // offline, so it must not make the mesh look actively syncing.
        #expect(o.syncingCount == 0)
        #expect(o.state == .idle)
        // The "last sync" line still reflects the most recent sync anywhere.
        #expect(o.mostRecentActivity == recent)
    }

    @Test func activityExactlyAtBoundaryIsNotActivelySyncing() {
        let now = Date()
        let o = SyncOverview.derive(
            meshRunning: true,
            knownPeers: ["a"],
            connectedPeers: ["a"],
            lastActivityByPeer: ["a": now.addingTimeInterval(-2)],
            now: now,
            syncingTimeout: 2
        )
        #expect(o.syncingCount == 0, "an activity exactly at the boundary has already relaxed")
        #expect(o.state == .idle)
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

@Suite("Scene background policy: active pairing window vs normal background")
struct SceneBackgroundPolicyTests {
    @Test func backgroundWhilePairingWindowActiveKeepsTheMesh() {
        #expect(SceneBackgroundPolicy.action(pairingWindowActive: true) == .keepMeshForPairing)
    }

    @Test func normalBackgroundWithoutWindowStopsTheMesh() {
        #expect(SceneBackgroundPolicy.action(pairingWindowActive: false) == .stopSync)
    }
}

@MainActor
@Suite("A pairing window stays discoverable through a bounded scene background")
struct SceneBackgroundPairingSessionTests {
    /// Records begin/expire/end calls so the scene-lifecycle logic can be
    /// verified without a live UIApplication.
    private final class SpyBackgroundTaskProvider: BackgroundTaskProviding {
        var grantsBudget = true
        /// What the fake provider reports as the granted mechanism, so the model
        /// can be tested distinguishing a full continued-processing handoff from
        /// the brief legacy fallback.
        var handleKind: BackgroundSessionKind = .continuedProcessing
        private(set) var beganCount = 0
        private(set) var endCount = 0
        private(set) var lastDurationSecs: Int?
        var expireHandlers: [@MainActor () -> Void] = []
        var lastHandle: BackgroundTaskHandle?

        func begin(durationSecs: Int, onExpire: @escaping @MainActor () -> Void) -> BackgroundTaskHandle? {
            beganCount += 1
            lastDurationSecs = durationSecs
            expireHandlers.append(onExpire)
            guard grantsBudget else { return nil }
            let handle = BackgroundTaskHandle(kind: handleKind) { [weak self] in self?.endCount += 1 }
            lastHandle = handle
            return handle
        }
    }

    private func makeModel(spy: SpyBackgroundTaskProvider) throws -> KiemModel {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("kiem-ios-scenebg-\(UUID().uuidString)", isDirectory: true)
        let model = try KiemModel(dataDir: dir)
        model.backgroundTaskProvider = spy
        return model
    }

    @Test func backgroundWhilePairingWindowActiveDoesNotStopTheMesh() async throws {
        let spy = SpyBackgroundTaskProvider()
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        model.armPairingWindow()
        #expect(model.pairingWindowIsActive)
        #expect(model.isSyncRunning)

        // The user copied the code and backgrounded the app to paste it elsewhere.
        model.handleSceneLeavingForeground()

        // The mesh must still be running and a bounded background session armed —
        // this is the regression the old unconditional stopSync() caused.
        #expect(model.isSyncRunning, "the pairing mesh must survive a scene background")
        #expect(spy.beganCount == 1, "a bounded background session should be requested")
        #expect(model.activeBackgroundSession != nil)
    }

    @Test func inactiveThenBackgroundTransitionArmsTheSessionExactlyOnce() async throws {
        let spy = SpyBackgroundTaskProvider()
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        model.armPairingWindow()
        // iOS delivers .inactive then .background in quick succession while a
        // foreground pairing window is open — both phases call the same
        // leaving-foreground handler.
        model.handleSceneLeavingForeground() // .inactive
        model.handleSceneLeavingForeground() // .background

        // The bounded session must survive the whole transition intact: armed
        // exactly once, and the second phase must not cancel/re-arm the
        // just-submitted continued-processing task (or restart its deadline).
        #expect(model.isSyncRunning, "the pairing mesh stays running across the transition")
        #expect(model.activeBackgroundSession != nil, "the session stays armed through the transition")
        #expect(spy.beganCount == 1, "the continued-processing task is submitted exactly once")
        #expect(spy.endCount == 0, "the transition must not cancel the in-flight task")
    }

    @Test func expiredBackgroundBudgetClosesWindowAndStopsTheMesh() async throws {
        let spy = SpyBackgroundTaskProvider()
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        model.armPairingWindow()
        model.handleSceneLeavingForeground()
        #expect(model.isSyncRunning)
        #expect(model.activeBackgroundSession != nil)

        // The system ends our background budget before the user finishes pasting.
        for onExpire in spy.expireHandlers { onExpire() }

        #expect(!model.isSyncRunning, "mesh must be torn down when the budget expires")
        #expect(!model.pairingWindowIsActive, "the window must close when the budget expires")
        #expect(model.activeBackgroundSession == nil)
        #expect(spy.endCount == 1, "the background task must be released")
    }

    @Test func normalBackgroundWithoutWindowStillStopsTheMesh() async throws {
        let spy = SpyBackgroundTaskProvider()
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        // No pairing window armed — plain backgrounding must behave exactly as
        // before: stop the mesh, no bounded background session.
        model.handleSceneLeavingForeground()

        #expect(!model.isSyncRunning)
        #expect(spy.beganCount == 0, "no pairing window means no background session")
    }

    @Test func returnToForegroundEndsTheBoundedSession() async throws {
        let spy = SpyBackgroundTaskProvider()
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        model.armPairingWindow()
        model.handleSceneLeavingForeground()
        #expect(model.activeBackgroundSession != nil)

        model.handleSceneReturnedToForeground()

        #expect(model.isSyncRunning, "the mesh is (re)started on return to foreground")
        #expect(model.activeBackgroundSession == nil, "the bounded session is released on return")
        #expect(spy.endCount == 1)
    }

    @Test func noGrantedBackgroundBudgetFallsBackToImmediateStop() async throws {
        let spy = SpyBackgroundTaskProvider()
        spy.grantsBudget = false
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        model.armPairingWindow()
        model.handleSceneLeavingForeground()

        // If the system grants no background time we must not leave the mesh
        // running backgrounded unwatched — close the window and stop.
        #expect(!model.isSyncRunning)
        #expect(!model.pairingWindowIsActive)
        #expect(spy.beganCount == 1)
    }

    @Test func backgroundSessionPassesTheFiveMinuteRemainingWindow() async throws {
        let spy = SpyBackgroundTaskProvider()
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        model.armPairingWindow()
        model.handleSceneLeavingForeground()

        // The provider is told how much pairing time is left so the
        // continued-processing path can report progress toward the full
        // five-minute handoff (and the model's deadline can cap it).
        #expect(spy.lastDurationSecs == Int(KiemModel.pairingWindowSecs))
    }

    @Test func closingTheWindowReleasesTheActiveBackgroundSession() async throws {
        let spy = SpyBackgroundTaskProvider()
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        model.armPairingWindow()
        model.handleSceneLeavingForeground()
        #expect(model.activeBackgroundSession != nil)

        // The user closed the pairing sheet (or the window otherwise shut): the
        // active background session must be released promptly so no dead handle
        // outlives the window it was keeping discoverable.
        model.closePairingWindow()
        #expect(model.activeBackgroundSession == nil, "closing the window must release the session")
        #expect(spy.endCount == 1, "the background session handle must be released on window close")
    }

    @Test func disappearKeepsWindowWhileBackgroundedAndClosesItInForeground() async throws {
        let spy = SpyBackgroundTaskProvider()
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        // Scene leaving with a live pairing session: a sheet disappear here is
        // the scene leaving, NOT the user dismissing — keep the window
        // discoverable so mid-handoff discovery isn't dropped.
        model.armPairingWindow()
        model.handleSceneLeavingForeground()
        #expect(model.activeBackgroundSession != nil)
        #expect(!model.shouldClosePairingWindowOnDisappear(),
                "a scene-background disappear must not close the pairing window")

        // Foreground with no session: a disappear is a real dismissal → close.
        model.handleSceneReturnedToForeground()
        #expect(model.activeBackgroundSession == nil)
        #expect(model.shouldClosePairingWindowOnDisappear(),
                "an active (foreground) dismissal must close the pairing window")
    }

    @Test func sessionKindDistinguishesContinuedHandoffFromBriefFallback() async throws {
        // A full continued-processing handoff keeps discovery alive for the whole
        // five-minute window; the brief legacy fallback only grants a short
        // grace period. The model must surface which one is in effect so the UI
        // never promises five minutes on a fallback.
        let continued = SpyBackgroundTaskProvider() // handleKind defaults .continuedProcessing
        let modelA = try makeModel(spy: continued)
        defer { modelA.shutDown() }
        modelA.armPairingWindow()
        modelA.handleSceneLeavingForeground()
        #expect(modelA.activeBackgroundSessionKind == .continuedProcessing)
        #expect(modelA.pairingBackgroundDisclosure == .continuedProcessing(remaining: Int(KiemModel.pairingWindowSecs)))

        let fallback = SpyBackgroundTaskProvider()
        fallback.handleKind = .briefFallback
        let modelB = try makeModel(spy: fallback)
        defer { modelB.shutDown() }
        modelB.armPairingWindow()
        modelB.handleSceneLeavingForeground()
        #expect(modelB.activeBackgroundSessionKind == .briefFallback)
        #expect(modelB.pairingBackgroundDisclosure == .briefFallback)
    }

    @Test func briefFallbackDisclosureDoesNotExposeTheFullWindow() {
        // A full continued-processing handoff may honestly report the remaining
        // window (here a full five-minute window) as background time…
        let continued = KiemModel.pairingBackgroundDisclosure(
            kind: .continuedProcessing,
            remaining: Int(KiemModel.pairingWindowSecs)
        )
        #expect(continued == .continuedProcessing(remaining: Int(KiemModel.pairingWindowSecs)))

        // …but the brief fallback is only a short best-effort grace period. It
        // must never carry (and so can never expose/format) the full-window
        // duration as a fallback budget. Equality to the value-less `.briefFallback`
        // case only compiles because the fallback carries no remaining value.
        let fallback = KiemModel.pairingBackgroundDisclosure(
            kind: .briefFallback,
            remaining: Int(KiemModel.pairingWindowSecs)
        )
        #expect(fallback == .briefFallback)
    }

    @Test func honestCopyDistinguishesMechanisms() {
        // The UI copy must not silently promise five background minutes when the
        // brief fallback (or foreground-only) is what's really available.
        #expect(KiemModel.pairingBackgroundCopy(for: .continuedProcessing)
                    .localizedCaseInsensitiveContains("background"))
        #expect(KiemModel.pairingBackgroundCopy(for: .briefFallback)
                    .localizedCaseInsensitiveContains("short"))
        #expect(!KiemModel.pairingBackgroundCopy(for: .briefFallback)
                    .localizedCaseInsensitiveContains("five minutes"))
        // Foreground-only result names the foreground, not the background.
        #expect(KiemModel.pairingBackgroundCopy(for: nil)
                    .localizedCaseInsensitiveContains("foreground"))
    }

    @Test func disappearDuringLeavingTransitionKeepsWindowEvenBeforeSessionArms() async throws {
        let spy = SpyBackgroundTaskProvider()
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        model.armPairingWindow()
        model.handleSceneLeavingForeground()
        // Simulate the onDisappear-before-session-arm ordering: the scene is
        // already transitioning out (scene flag set) but the bounded session has
        // not yet been stored (the arm may still be in flight or was released).
        model.endActiveBackgroundSession()
        #expect(model.activeBackgroundSession == nil)
        #expect(!model.shouldClosePairingWindowOnDisappear(),
                "a disappear while the scene is leaving must not close the window, even before the session handle is armed")
    }

    @Test func foregroundDismissalClosesWindowEvenWithStaleActiveHandle() async throws {
        let spy = SpyBackgroundTaskProvider()
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        model.armPairingWindow()
        model.handleSceneLeavingForeground()
        #expect(model.activeBackgroundSession != nil)
        #expect(model.sceneIsLeavingForeground)

        // The scene is back in the foreground, but — in this regression — a
        // stale active handle is still held. The explicit scene flag is the
        // authoritative distinction: a real (foreground) dismissal must close
        // the pairing window even though the stale handle remains; the handle
        // must never mask a genuine dismissal.
        model.sceneIsLeavingForeground = false
        #expect(model.activeBackgroundSession != nil, "the stale handle is still held")
        #expect(model.shouldClosePairingWindowOnDisappear(),
                "a foreground dismissal must close the window even with a stale active handle")
    }

    @Test func lateExpirationAfterForegroundReturnDoesNotTearDownForegroundMesh() async throws {
        let spy = SpyBackgroundTaskProvider()
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        model.armPairingWindow()
        model.handleSceneLeavingForeground()
        #expect(model.activeBackgroundSession != nil)

        model.handleSceneReturnedToForeground()
        #expect(model.isSyncRunning, "foreground mesh is running again")
        #expect(model.pairingWindowIsActive)

        // A stale expiration from the ended background session arrives late (the
        // legacy fallback's beginBackgroundTask completion can fire after the app
        // already returned to foreground). It must not tear the now-foreground
        // pairing down.
        for onExpire in spy.expireHandlers { onExpire() }

        #expect(model.isSyncRunning, "a late expiration must not stop the foreground mesh")
        #expect(model.pairingWindowIsActive, "a late expiration must not close the foreground window")
        #expect(model.pairingRequest == nil)
    }

    @Test func expiredSessionResolvesPendingApprovalAsDenied() async throws {
        let spy = SpyBackgroundTaskProvider()
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        model.armPairingWindow()
        model.requestPairingApproval(peerId: "peer-late", gate: ApprovalGate())
        let gate = model.pairingRequest?.gate
        #expect(model.pairingRequest?.peerId == "peer-late")

        model.handleSceneLeavingForeground()
        for onExpire in spy.expireHandlers { onExpire() }

        #expect(gate?.wait() == false, "expiration must resolve the blocked approval gate to deny")
        #expect(model.pairingRequest == nil)
    }

    @Test func repeatedExpirationIsIdempotent() async throws {
        let spy = SpyBackgroundTaskProvider()
        let model = try makeModel(spy: spy)
        defer { model.shutDown() }

        model.armPairingWindow()
        model.handleSceneLeavingForeground()
        // The system can deliver more than one expiration (continued task then
        // fallback, or a retried callback); each must be safe to re-enter.
        for _ in 0..<2 { for onExpire in spy.expireHandlers { onExpire() } }

        #expect(!model.isSyncRunning)
        #expect(!model.pairingWindowIsActive)
        #expect(spy.endCount == 1, "the handle must be released exactly once")
    }
}

@Suite("Approval gate timeout aligns with the five-minute pairing window")
struct ApprovalGateTimeoutTests {
    @Test func defaultTimeoutMatchesThePairingWindow() {
        // A genuinely pending approval (a handoff kept discoverable through the
        // background) deserves the whole five-minute window to be answered, and
        // never blocks the sync thread past it.
        #expect(ApprovalGate.defaultTimeout == TimeInterval(KiemModel.pairingWindowSecs))
    }

    @Test func fiveMinuteApprovalTimeout() {
        #expect(ApprovalGate.defaultTimeout == 300)
    }
}

@Suite("BGTaskScheduler/UIApplication callbacks stay nonisolated (0.4.1(6) backgrounding crash)")
struct BackgroundSessionCallbackIsolationTests {
    /// `@unchecked Sendable`: mutated only by the single @MainActor work closures
    /// below and read strictly after the hop is observed to land.
    private final class Observation: @unchecked Sendable {
        var ranOnNonMainThread = false
        var ranOnMainThread = false
        var didRun = false
    }

    /// Drive a production off-main OS callback from a genuinely non-main (detached)
    /// thread, then await a hop-landed sentinel routed through the same
    /// `dispatchLaunch` seam every OS trampoline uses. Asserts the trampoline never
    /// ran @MainActor work synchronously on the caller thread (the 0.4.1(6) trap),
    /// and that the shared hop landed on main.
    ///
    /// Why this is red-capable for the exact bug: if any of these production
    /// callbacks were re-introduced as an inline closure literal formed inside a
    /// @MainActor method (as in build 6), Swift 6 infers it @MainActor-isolated and
    /// invoking it on this detached thread traps with `swift_task_checkIsolatedSwift`
    /// (SIGILL/EXC_BREAKPOINT), crashing — and thereby failing — the test suite.
    private func assertHopsWhenInvokedOffMain(_ rawCall: @escaping @Sendable () -> Void) async {
        let obs = Observation()
        let landed = AsyncStream<Void>.makeStream()
        await withCheckedContinuation { (entered: CheckedContinuation<Void, Never>) in
            Thread.detachNewThread {
                rawCall() // invoke the production trampoline on this off-main thread (must not trap)
                // Sentinel through the shared hop seam the trampolines all use.
                DefaultBackgroundSessionProvider.dispatchLaunch {
                    if !Thread.isMainThread { obs.ranOnNonMainThread = true }
                    obs.didRun = true
                    obs.ranOnMainThread = Thread.isMainThread
                    landed.continuation.yield()
                }
                entered.resume()
            }
        }
        _ = await landed.stream.first { _ in true }
        #expect(obs.didRun, "the hop sentinel must eventually run on main")
        #expect(!obs.ranOnNonMainThread,
                "the off-main callback must not run MainActor work synchronously on the caller thread (the 0.4.1(6) isolation-trap crash)")
        #expect(obs.ranOnMainThread, "the off-main callback's work must run on the main actor after the hop")
    }

    /// The shared hop seam (`dispatchLaunch`) — the mechanism every OS trampoline
    /// routes through — defers @MainActor work to main when invoked off-main.
    @Test func launchSeamDefersMainActorWorkWhenInvokedOffMain() async {
        await assertHopsWhenInvokedOffMain {
            DefaultBackgroundSessionProvider.dispatchLaunch { /* inert */ }
        }
    }

    /// The actual registered launch handler path: `ensureRegistered()` passes the
    /// direct nonisolated function reference `Self.handleLaunchedTask`, whose body
    /// (`handleLaunchedTask → launchBoxed`) routes the boxed system task into the
    /// hop. A `BGTask` cannot be constructed on the Simulator, so this drives the
    /// exact registered-handler body (`launchBoxed`) from a non-main thread with a
    /// boxed-`nil` (absent/wrong-type) task — proving it is nonisolated off-main
    /// and completes defensively through the hop.
    @Test func productionLaunchTrampolineHopsWhenInvokedOffMain() async {
        await assertHopsWhenInvokedOffMain {
            DefaultBackgroundSessionProvider.launchBoxed(TaskValueBox(nil))
        }
    }

    /// The continued-processing `expirationHandler` (assigned as the nonisolated
    /// function reference `Self.handleContinuedExpirationTrampoline`) stays
    /// nonisolated and hops when the scheduler invokes it off-main (completion queue).
    @Test func continuedExpirationTrampolineHopsWhenInvokedOffMain() async {
        await assertHopsWhenInvokedOffMain {
            DefaultBackgroundSessionProvider.handleContinuedExpirationTrampoline()
        }
    }

    /// The legacy `beginBackgroundTask` `expirationHandler` (assigned as the
    /// nonisolated function reference `Self.handleFallbackExpirationTrampoline`)
    /// stays nonisolated and hops when UIApplication invokes it off-main
    /// (expiration queue).
    @Test func fallbackExpirationTrampolineHopsWhenInvokedOffMain() async {
        await assertHopsWhenInvokedOffMain {
            DefaultBackgroundSessionProvider.handleFallbackExpirationTrampoline()
        }
    }
}

@Suite("Markdown editor renderer (Pulp-powered rich inline text without losing source)")
struct MarkdownEditorRendererTests {
    private static func attr(_ key: NSAttributedString.Key, at index: Int, in s: NSAttributedString) -> Any? {
        var effective = NSRange(location: 0, length: 0)
        return s.attributes(at: index, effectiveRange: &effective)[key]
    }

    /// The rendered (styled) string must keep the exact Markdown source so edits
    /// round-trip through the version-aware write unchanged — styling must never
    /// rewrite characters.
    @Test func renderedStringPreservesExactMarkdownSource() {
        let src = "# Title\n\nUse `code` in **bold**.\n\n- [ ] a task\n"
        #expect(MarkdownEditorRenderer.styledText(src).string == src)
    }

    @Test func headingIsRenderedLargeAndMarkerIsShrunk() {
        let styled = MarkdownEditorRenderer.styledText("# Hello")
        let markerFont = Self.attr(.font, at: 0, in: styled) as? UIFont
        let contentFont = Self.attr(.font, at: 2, in: styled) as? UIFont
        #expect(markerFont?.pointSize ?? 999 < 1,
                "the '#' marker must be shrunk to near-invisible so prose reads clean")
        #expect(contentFont?.pointSize ?? 0 > 16,
                "heading content must render larger than the 16pt body font")
    }

    @Test func taskItemMarkerIsInvisible() {
        let styled = MarkdownEditorRenderer.styledText("- [ ] do it")
        let markerColor = Self.attr(.foregroundColor, at: 0, in: styled) as? UIColor
        #expect(UIColor.clear.isEqual(markerColor),
                "the \"- [ ]\" syntax must be invisible so the checkbox row reads as content")
        #expect(styled.string == "- [ ] do it", "the source must survive marker hiding")
    }

    @Test func inlineCodeCarriesACodeBackground() {
        let styled = MarkdownEditorRenderer.styledText("Use `code` here")
        #expect(Self.attr(.backgroundColor, at: 5, in: styled) != nil,
                "inline code content must carry a code-background fill")
    }
}
