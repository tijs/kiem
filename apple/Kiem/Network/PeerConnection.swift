import Foundation
import KiemKit
import Network

/// One peer-to-peer sync link over a single `NWConnection`. Drives the Kiem
/// handshake and the Automerge sync loop, mirroring the CLI daemon
/// (`crates/kiem-cli/src/daemon.rs`).
///
/// All work runs on the shared serial `queue` the manager owns, so the link
/// needs no internal locking; `KiemStore` is `Sendable`, so its sync calls are
/// safe here and keep the Rust work off the main thread.
final class PeerConnection {
    /// The remote peer's id, learned from its hello frame. nil until handshaken.
    private(set) var remotePeerId: String?

    private let connection: NWConnection
    private let store: KiemStore
    private let localPeerId: String
    private let queue: DispatchQueue

    private var decoder = SyncFrameDecoder()
    private var ticker: DispatchSourceTimer?
    private var closed = false
    /// Whether closing should drop this peer's sync state. Set false when the
    /// manager rejects a duplicate link, so the surviving link keeps its state.
    private var forgetOnClose = true

    /// Called once the remote peer id is known. Return `true` to keep the link,
    /// `false` to drop it (duplicate). Runs on `queue`.
    var onIdentified: ((String, PeerConnection) -> Bool)?
    /// Called once when the link closes. Runs on `queue`.
    var onClosed: ((PeerConnection) -> Void)?

    /// How often to push pending sync messages for every document — the CLI
    /// daemon's default cadence.
    private static let syncInterval: DispatchTimeInterval = .milliseconds(200)

    init(connection: NWConnection, store: KiemStore, localPeerId: String, queue: DispatchQueue) {
        self.connection = connection
        self.store = store
        self.localPeerId = localPeerId
        self.queue = queue
    }

    func start() {
        connection.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            switch state {
            case .ready:
                self.sendHello()
                self.receiveLoop()
            case .failed, .cancelled:
                self.close()
            default:
                break
            }
        }
        connection.start(queue: queue)
    }

    /// Tear down the link. Idempotent. Drops this peer's sync state unless a
    /// duplicate-reject asked to keep it.
    func close() {
        guard !closed else { return }
        closed = true
        ticker?.cancel()
        ticker = nil
        connection.cancel()
        if forgetOnClose, let peer = remotePeerId {
            store.forgetPeer(peerId: peer)
        }
        onClosed?(self)
    }

    // MARK: - Handshake

    private func sendHello() {
        send(SyncFrame.control(peerId: localPeerId))
    }

    private func identify(_ peerId: String) {
        // The first control frame carries the remote peer id. Accept one only.
        guard remotePeerId == nil else { return }
        guard !peerId.isEmpty, peerId != localPeerId else {
            close()
            return
        }
        remotePeerId = peerId
        if onIdentified?(peerId, self) == false {
            // Already linked to this peer on another connection: drop this one
            // without forgetting the shared sync state.
            forgetOnClose = false
            close()
            return
        }
        startTicker()
    }

    // MARK: - Receive

    private func receiveLoop() {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, isComplete, error in
            guard let self else { return }
            if let data, !data.isEmpty {
                self.ingest(data)
            }
            if let error {
                debugPrint("Kiem sync receive error: \(error)")
                self.close()
                return
            }
            if isComplete {
                self.close()
                return
            }
            if !self.closed { self.receiveLoop() }
        }
    }

    private func ingest(_ data: Data) {
        decoder.append(data)
        do {
            while let frame = try decoder.next() {
                handle(frame)
            }
        } catch {
            debugPrint("Kiem sync decode error: \(error)")
            close()
        }
    }

    private func handle(_ frame: SyncFrame) {
        if frame.isControl {
            if let peerId = String(data: frame.payload, encoding: .utf8) {
                identify(peerId)
            }
            return
        }
        // Data frame: apply it, then reply immediately if a message is pending.
        guard let peer = remotePeerId else { return }
        do {
            try store.receiveSyncMessage(peerId: peer, docId: frame.docId, message: frame.payload)
            if let reply = try store.generateSyncMessage(peerId: peer, docId: frame.docId) {
                send(SyncFrame(docId: frame.docId, payload: reply))
            }
        } catch {
            debugPrint("Kiem sync apply error for doc \(frame.docId): \(error)")
        }
    }

    // MARK: - Ticker (push every document)

    private func startTicker() {
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + Self.syncInterval, repeating: Self.syncInterval)
        timer.setEventHandler { [weak self] in self?.syncRound() }
        ticker = timer
        timer.resume()
        syncRound() // push an initial round immediately on link-up
    }

    private func syncRound() {
        guard let peer = remotePeerId, !closed else { return }
        do {
            for docId in try store.getDocumentIds() {
                if let payload = try store.generateSyncMessage(peerId: peer, docId: docId) {
                    send(SyncFrame(docId: docId, payload: payload))
                }
            }
        } catch {
            debugPrint("Kiem sync round error: \(error)")
        }
    }

    // MARK: - Send

    private func send(_ frame: SyncFrame) {
        connection.send(content: frame.encoded(), completion: .contentProcessed { [weak self] error in
            if let error {
                debugPrint("Kiem sync send error: \(error)")
                self?.close()
            }
        })
    }
}
