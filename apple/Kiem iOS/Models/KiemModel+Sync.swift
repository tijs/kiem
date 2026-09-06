import Foundation
import KiemKit
import Pulp

/// Joining the P2P mesh and pairing devices: everything the pairing/sync UI
/// drives. Same behaviour as the macOS `KiemModel+Sync.swift`; no AppKit.
extension KiemModel {
    /// Apply a body's derived metadata to a note (used by the editor header
    /// preview and by the todo toggle). The authoritative derivation happens
    /// in Rust at flush time; this is only for display. Wraps Pulp's
    /// platform-neutral `ContentAnalyzer`; `nonisolated` and pure so unit
    /// tests can drive it off the main actor.
    nonisolated static func derive(titleFrom body: String) -> String {
        ContentAnalyzer.extractTitle(from: body)
    }

    nonisolated static func derive(tagsFrom body: String) -> [String] {
        ContentAnalyzer.extractTags(from: body)
    }

    nonisolated static func derive(hasUncheckedTodosFrom body: String) -> Bool {
        ContentAnalyzer.hasUncheckedTodos(in: body)
    }

    // MARK: Pairing display helpers (pure, shared with the Mac sheet)

    /// How a peer's *displayed* status is derived from connectivity and recent
    /// sync activity. UI-facing only, so it lives as a testable pure function.
    enum PeerStatus: Equatable {
        case offline
        case connected
        case syncing
    }

    /// How long a connected peer is shown as "Syncing" after its last sync
    /// activity before it relaxes to "Connected".
    static let peerSyncingTimeout: TimeInterval = 2

    /// `m:ss` countdown formatting for the pairing window (same as the Mac
    /// sheet's `mmss`). `nonisolated` and pure so unit tests can call it
    /// off the main actor.
    nonisolated static func mmss(_ secs: Int) -> String {
        String(format: "%d:%02d", secs / 60, secs % 60)
    }

    /// Whether a pairing window is currently discoverable: `remaining > 0`.
    /// The pairing ticket is only valid — and only presentable — while this
    /// holds, so an expired (0) or unset (nil) window must not leave a stale
    /// code scannable or shareable. `nonisolated` and pure so unit tests can
    /// drive it off the main actor.
    nonisolated static func pairingWindowIsActive(remaining: Int?) -> Bool {
        guard let remaining else { return false }
        return remaining > 0
    }

    /// A connected peer with sync activity inside `syncingTimeout` shows as
    /// "Syncing"; connected-but-idle shows as "Connected"; disconnected is
    /// "Offline" regardless of stale activity. `nonisolated` and pure.
    nonisolated static func peerStatus(
        isConnected: Bool,
        lastActivity: Date?,
        now: Date,
        syncingTimeout: TimeInterval
    ) -> PeerStatus {
        guard isConnected else { return .offline }
        if let lastActivity, now.timeIntervalSince(lastActivity) < syncingTimeout {
            return .syncing
        }
        return .connected
    }

    /// This device's view of `peerId`'s status at a given moment. `now` is
    /// passed in (rather than read internally) so the view can drive a
    /// re-render from its own observable clock ticks; feeding it via the
    /// `peerStatus(isConnected:lastActivity:now:syncingTimeout:)` helper below
    /// keeps the derivation pure and unit-testable.
    func peerStatus(for peerId: String, now: Date) -> PeerStatus {
        Self.peerStatus(
            isConnected: connectedPeers.contains(peerId),
            lastActivity: lastSyncActivity[peerId],
            now: now,
            syncingTimeout: Self.peerSyncingTimeout
        )
    }

    /// Start (or restart) the Rust-backed sync mesh. Idempotent: a second call
    /// while a mesh is already armed is a no-op, so the scene-phase handler can
    /// safely call this on every return-to-active (it only actually re-arms the
    /// mesh when a background/inactive pause stopped it). The arm is
    /// asynchronous; `isSyncRunning` flips immediately so repeated calls within
    /// that window are coalesced, and reverts if the Rust start fails.
    func startSync() {
        guard !isSyncRunning else { return }
        isSyncRunning = true
        knownPeers = (try? store.knownPeers()) ?? []
        deviceName = store.deviceName()
        let events = SyncPeerEvents(
            onChange: { [weak self] peerId, connected in
                Task { @MainActor in
                    guard let self else { return }
                    var peers = Set(self.connectedPeers)
                    if connected { peers.insert(peerId) } else { peers.remove(peerId) }
                    self.connectedPeers = peers.sorted()
                    self.knownPeers = (try? self.store.knownPeers()) ?? []
                }
            },
            onActivity: { [weak self] peerId in
                Task { @MainActor in
                    guard let self else { return }
                    self.lastSyncActivity[peerId] = Date()
                    // Incoming sync writes land in the store; nudge a refresh so
                    // the list/editor catch up promptly (foreground sync only).
                    self.scheduleDebouncedRefresh()
                }
            },
            onApprove: { [weak self] peerId in
                guard let self else { return false }
                let gate = ApprovalGate()
                Task { @MainActor in
                    self.requestPairingApproval(peerId: peerId, gate: gate)
                }
                return gate.wait()
            }
        )
        let store = self.store
        let gate = self.syncLifecycleGate
        let queue = self.syncLifecycleQueue
        let generation = gate.requestStart()
        // The actual Rust arm runs on a serial lifecycle queue so it can never
        // race stopSync() (which enqueues behind it). The generation gate makes
        // the arm abort if a background/inactive stop superseded it while the
        // block was waiting — so a scene that goes away mid-start can't leave
        // the mesh running in the background, nor can an old start's failure
        // tear down a newer mesh.
        queue.async {
            guard gate.isCurrentStart(generation) else { return }
            do {
                try store.startSync(intervalMs: 1000, events: events)
            } catch {
                debugPrint("kiem iOS sync failed to start: \(error)")
                Task { @MainActor [weak self] in
                    // Only un-arm if this start is still the current one, so a
                    // stale start's failure can't take down a newer mesh.
                    if gate.shouldRevert(generation) {
                        self?.isSyncRunning = false
                    }
                }
            }
        }
    }

    /// Stop the mesh. Idempotent: stopping an already-stopped mesh is a no-op.
    /// The actual Rust stop is enqueued behind any pending start on the same
    /// serial lifecycle queue, so the mesh is never left armed after a
    /// background/inactive pause.
    func stopSync() {
        guard isSyncRunning else { return }
        isSyncRunning = false
        let store = self.store
        syncLifecycleGate.requestStop()
        syncLifecycleQueue.async {
            store.stopSync()
        }
        connectedPeers = []
    }

    var shortDeviceId: String {
        String(authorDid.prefix(12))
    }

    private static let pairingWindowSecs: UInt64 = 120

    /// Open the pairing window and start showing a code. The arm is optimistically
    /// reflected in `pairingWindowRemaining` so the sheet doesn't flash a spinner;
    /// `refreshPairingWindow` then reconciles with the store. Because the Rust arm
    /// is a no-op until the mesh is running, `refreshPairingWindow` re-arms on
    /// every tick until the store confirms the window is live (see
    /// `wantsPairingWindow`), so a sheet presented during async sync startup still
    /// reliably becomes discoverable.
    func armPairingWindow() {
        // Bump the generation so any in-flight ticket fetch from a previous arm
        // or close is dropped, then record that the sheet wants a window.
        pairingGeneration &+= 1
        let generation = pairingGeneration
        wantsPairingWindow = true
        store.armPairing(windowSecs: Self.pairingWindowSecs)
        pairingWindowRemaining = Int(Self.pairingWindowSecs)
        fetchPairingTicket(generation: generation)
    }

    func closePairingWindow() {
        wantsPairingWindow = false
        // Invalidate any in-flight ticket fetch / retry: once the sheet is closed
        // a previously-started fetch must not resurrect a code.
        pairingGeneration &+= 1
        pairingTicket = nil
        pairingWindowRemaining = nil
        store.armPairing(windowSecs: 0)
    }

    /// Whether the current pairing window is discoverable (see the pure
    /// `pairingWindowIsActive(remaining:)` helper). Drives ticket visibility so
    /// an expired code stops being shown.
    var pairingWindowIsActive: Bool {
        Self.pairingWindowIsActive(remaining: pairingWindowRemaining)
    }

    /// Fetches this device's pairing ticket off the main thread (the relay wait
    /// is bounded), then publishes it only if the window that asked for it is
    /// still the current one. The generation check drops a fetch that started
    /// under a superseded arm — e.g. the sheet was closed and reused before the
    /// standalone ticket landed — so a stale arm can never resurrect a code for
    /// a window that is no longer open. Only a non-nil result replaces the ticket:
    /// a transient fetch failure must not erase a code that's already presentable
    /// (expiry/close clearing is owned by `refreshPairingWindow`/`closePairingWindow`).
    private func fetchPairingTicket(generation: UInt64) {
        let store = self.store
        Task.detached {
            if let ticket = try? store.pairTicket() {
                await MainActor.run {
                    guard self.pairingGeneration == generation else { return }
                    self.pairingTicket = ticket
                }
            }
        }
    }

    func refreshPairingWindow() {
        let remaining = store.pairingWindowRemaining().map(Int.init)
        pairingWindowRemaining = remaining
        if let remaining, remaining > 0 {
            // The window is actually armed and live: the sheet's intent is
            // satisfied. Clear it so a later expiry still falls through to the
            // "make discoverable again" button instead of the sheet re-arming an
            // already-consumed window forever.
            wantsPairingWindow = false
            // The window is active but take care that a ticket is actually
            // present. When the mesh is already up, `pair_ticket` uses the live
            // endpoint path, whose relay-registration wait can take longer than
            // the undo-armed arm retry — keep nudging the fetch until it lands so
            // a slow ticket never leaves the sheet code-less. (Gen-guarded, and
            // no re-arm here so expiry semantics are preserved.)
            if pairingTicket == nil {
                fetchPairingTicket(generation: pairingGeneration)
            }
            return
        }
        guard wantsPairingWindow else {
            // Genuinely closed or expired: a residue ticket is stale and must no
            // longer be scannable or shareable. Clearing here — independent of the
            // view's own `pairingWindowIsActive` guard — prevents stale visibility
            // entirely.
            pairingTicket = nil
            return
        }
        // The window reports closed. That means the mesh is most likely still
        // starting, so the earlier `arm_pairing` was a no-op. Re-arm until the
        // store confirms it; keep the optimistic `pairingWindowRemaining` so the
        // code stays visible through startup. Only fetch a fresh ticket if we
        // don't already have one — the arm taking effect in the mesh is what the
        // retry is waiting on, not the ticket.
        store.armPairing(windowSecs: Self.pairingWindowSecs)
        pairingWindowRemaining = Int(Self.pairingWindowSecs)
        if pairingTicket == nil {
            fetchPairingTicket(generation: pairingGeneration)
        }
    }

    func addDevice(ticket: String) {
        let trimmed = ticket.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        // Starting a fresh pairing action clears any stale error (from a
        // failed add, rename or unpair) so it doesn't linger on the next
        // attempt and resurface in the underlying list alert.
        errorMessage = nil
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

    func peerName(for peerId: String) -> String {
        store.peerName(peerId: peerId)
    }

    /// Rename this device. Returns `true` on success so the caller can keep an
    /// edit surface open on failure rather than discard the user's draft. On
    /// failure a descriptive `errorMessage` is set (surfaced by the pairing
    /// sheet); on success `deviceName` is updated. The empty/whitespace guard
    /// returns `false` without touching state. (iOS edition — the macOS
    /// implementation is separate and intentionally unchanged.)
    @discardableResult
    func setDeviceName(_ name: String) -> Bool {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return false }
        do {
            try store.setDeviceName(name: trimmed)
            deviceName = trimmed
            return true
        } catch {
            errorMessage = "Couldn't rename this device: \(error)"
            return false
        }
    }

    func requestPairingApproval(peerId: String, gate: ApprovalGate) {
        guard pairingRequest == nil else {
            gate.resolve(false)
            return
        }
        pairingRequest = PairingRequest(peerId: peerId, gate: gate)
    }

    func resolvePairing(_ allow: Bool) {
        guard let request = pairingRequest else { return }
        pairingRequest = nil
        request.gate.resolve(allow)
        if allow {
            knownPeers = (try? store.knownPeers()) ?? []
        }
    }
}

/// A pending incoming pairing shown to the user as Allow/Deny. Holds the
/// `ApprovalGate` the sync thread is blocked on until they decide.
struct PairingRequest: Identifiable {
    let id = UUID()
    let peerId: String
    let gate: ApprovalGate

    var shortPeerId: String {
        String(peerId.prefix(12))
    }
}

extension KiemModel {
    /// The single alert's message: a pending pairing prompt wins; otherwise the
    /// last error.
    var pairingMessage: String {
        if let request = pairingRequest {
            return "“\(peerName(for: request.peerId))” (\(request.shortPeerId)) wants to pair with this device."
        }
        return errorMessage ?? ""
    }
}

/// Blocks the Rust `approve_pairing` thread until the UI resolves it. `wait()`
/// is bounded: if the UI disappears/backgrounds and never answers, it resolves
/// to deny and returns after `timeout`, so the sync thread can never be parked
/// forever. Result access is lock-guarded (it's read on a Rust thread and
/// written on the main actor); the first resolution wins.
final class ApprovalGate: @unchecked Sendable {
    /// Default upper bound on how long a pairing request may block the sync
    /// thread. If the pairing sheet is dismissed or the scene goes away, the
    /// request is treated as denied after this so the Rust caller can proceed.
    static let defaultTimeout: TimeInterval = 120

    private let timeout: TimeInterval
    private let semaphore = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var result = false
    private var resolved = false

    init(timeout: TimeInterval = ApprovalGate.defaultTimeout) {
        self.timeout = timeout
    }

    /// Thread-safe snapshot of the resolved decision.
    private var currentResult: Bool {
        lock.lock(); defer { lock.unlock() }
        return result
    }

    /// Resolve the gate with the user's Allow/Deny. The first resolution wins;
    /// a late call (e.g. a timeout racing the user's tap) is ignored so a
    /// timeout can't override a decision the user actually made.
    func resolve(_ allow: Bool) {
        lock.lock()
        defer { lock.unlock() }
        guard !resolved else { return }
        resolved = true
        result = allow
        semaphore.signal()
    }

    /// Block up to `timeout` seconds for the UI to resolve. Returns the
    /// decision; on timeout it resolves to deny (so the sync thread proceeds)
    /// and returns false.
    func wait() -> Bool {
        if semaphore.wait(timeout: .now() + timeout) == .timedOut {
            resolve(false)
        }
        return currentResult
    }
}

/// Thread-safe gate serializing the sync start/stop lifecycle. It records the
/// current "desired running" state and bumps a generation on every start/stop
/// request, so a pending (queued) start that is superseded by a stop — or a
/// stale start whose failure handler runs late — can detect it and back off
/// instead of arming a mesh that should be stopped.
final class SyncLifecycleGate: @unchecked Sendable {
    private let lock = NSLock()
    private var generation: UInt64 = 0
    private var desiredRunning = false

    /// Record a request to run the mesh and return the generation this start
    /// is authorized for.
    func requestStart() -> UInt64 {
        lock.lock(); defer { lock.unlock() }
        generation += 1
        desiredRunning = true
        return generation
    }

    /// Record a request to stop the mesh; invalidates any in-flight start of an
    /// earlier generation.
    func requestStop() {
        lock.lock(); defer { lock.unlock() }
        generation += 1
        desiredRunning = false
    }

    /// Whether a start authorized at `generation` still reflects the current
    /// intent. A start that was superseded (by a stop or a newer start) must
    /// not arm the mesh.
    func isCurrentStart(_ generation: UInt64) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return desiredRunning && self.generation == generation
    }

    /// Whether the start authorized at `generation` is still current, i.e. its
    /// failure should be reflected by un-arming `isSyncRunning`. An old start's
    /// failure must not take down a newer mesh.
    func shouldRevert(_ generation: UInt64) -> Bool {
        isCurrentStart(generation)
    }
}

/// Bridges `kiem-sync` mesh callbacks (arriving on Rust threads) to Sendable
/// closures; the model hops them onto the main actor.
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

    func onConnected(peerId: String) { onChange(peerId, true) }
    func onDisconnected(peerId: String) { onChange(peerId, false) }
    func onSyncActivity(peerId: String) { onActivity(peerId) }
    func approvePairing(peerId: String) -> Bool { onApprove(peerId) }
}
