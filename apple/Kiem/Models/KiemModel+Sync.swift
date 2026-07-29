import Foundation
import KiemKit

/// Joining the P2P mesh and pairing devices: everything the Sync settings pane
/// drives. The observable state it writes to (`connectedPeers`, `knownPeers`,
/// `pairingTicket`, …) is declared on the class in `KiemModel.swift` — Swift
/// extensions cannot add stored properties.
extension KiemModel {
    /// Begin P2P sync: join the iroh mesh in the Rust core (`kiem-sync`), the
    /// same transport and known-peers store the CLI daemon uses. Peers are
    /// added by ticket pairing (`kiem pair`); incoming sync writes land in the
    /// shared SQLite store, where the DB watcher picks them up for the UI.
    func startSync() {
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

    /// This device's endpoint id, shortened to the same length the other side's
    /// Allow/Deny prompt shows — so the two can be compared by eye.
    var shortDeviceId: String {
        String(authorDid.prefix(12))
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
