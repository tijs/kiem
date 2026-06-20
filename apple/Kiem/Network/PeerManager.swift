import Foundation
import KiemKit
import Network

/// A peer currently linked for sync, surfaced to the UI.
struct ConnectedPeer: Identifiable, Hashable, Sendable {
    /// The remote peer id (a UUID); also the Bonjour instance name.
    let id: String
}

/// Discovers Kiem peers on the local network (Bonjour `_kiem._tcp`), accepts and
/// dials connections, and keeps one converged `PeerConnection` per peer. Mirrors
/// the CLI daemon so app and CLI peers interoperate on the same service.
///
/// All networking state lives on one serial queue; the only hop off it is
/// `onPeersChanged`, which the owner forwards to the UI on the main actor.
final class PeerManager {
    let localPeerId: String

    private let store: KiemStore
    private let queue = DispatchQueue(label: "org.tijs.kiem.sync")
    private static let serviceType = "_kiem._tcp"

    private var listener: NWListener?
    private var browser: NWBrowser?

    /// Every connection we retain, by identity — including not-yet-identified ones.
    private var allLinks: [ObjectIdentifier: PeerConnection] = [:]
    /// Identified, accepted links by remote peer id (the dedup + UI source).
    private var byPeer: [String: PeerConnection] = [:]
    /// Peer ids we have an outbound dial in flight for, to avoid double-dialing
    /// before the link identifies.
    private var dialing: Set<String> = []
    private var dialingByLink: [ObjectIdentifier: String] = [:]

    /// Fired on the internal queue whenever the connected-peer set changes.
    var onPeersChanged: (([ConnectedPeer]) -> Void)?

    init(store: KiemStore, localPeerId: String) {
        self.store = store
        self.localPeerId = localPeerId
    }

    deinit {
        // No other references remain, so cancel directly (cancel() is
        // thread-safe) rather than hopping through the queue with a weak self.
        listener?.cancel()
        browser?.cancel()
        for link in allLinks.values { link.close() }
    }

    func start() {
        queue.async { [weak self] in
            self?.startListener()
            self?.startBrowser()
        }
    }

    func stop() {
        queue.async { [weak self] in
            guard let self else { return }
            self.browser?.cancel(); self.browser = nil
            self.listener?.cancel(); self.listener = nil
            for link in self.allLinks.values { link.close() }
        }
    }

    // MARK: - Listener (advertise + accept)

    private func startListener() {
        do {
            let listener = try NWListener(using: .tcp)
            listener.service = NWListener.Service(name: localPeerId, type: Self.serviceType)
            listener.stateUpdateHandler = { state in
                if case let .failed(error) = state {
                    debugPrint("Kiem listener failed: \(error)")
                }
            }
            listener.newConnectionHandler = { [weak self] connection in
                self?.adopt(connection)
            }
            listener.start(queue: queue)
            self.listener = listener
        } catch {
            debugPrint("Kiem listener start error: \(error)")
        }
    }

    // MARK: - Browser (discover + dial)

    private func startBrowser() {
        let browser = NWBrowser(for: .bonjour(type: Self.serviceType, domain: nil), using: NWParameters())
        browser.stateUpdateHandler = { state in
            if case let .failed(error) = state {
                debugPrint("Kiem browser failed: \(error)")
            }
        }
        browser.browseResultsChangedHandler = { [weak self] results, _ in
            self?.handleBrowse(results)
        }
        browser.start(queue: queue)
        self.browser = browser
    }

    private func handleBrowse(_ results: Set<NWBrowser.Result>) {
        for result in results {
            guard case let .service(name, _, _, _) = result.endpoint else { continue }
            // Lexicographic dedup: only the smaller peer-id dials; the larger
            // just listens. Matches the CLI (`dialer.peer_id > instance` skips).
            if name == localPeerId || localPeerId > name { continue }
            if byPeer[name] != nil || dialing.contains(name) { continue }
            dial(result.endpoint, expectedPeerId: name)
        }
    }

    private func dial(_ endpoint: NWEndpoint, expectedPeerId: String) {
        adopt(NWConnection(to: endpoint, using: .tcp), expectedPeerId: expectedPeerId)
    }

    // MARK: - Link lifecycle

    private func adopt(_ connection: NWConnection, expectedPeerId: String? = nil) {
        let link = PeerConnection(connection: connection, store: store, localPeerId: localPeerId, queue: queue)
        let oid = ObjectIdentifier(link)
        allLinks[oid] = link
        if let expectedPeerId {
            dialing.insert(expectedPeerId)
            dialingByLink[oid] = expectedPeerId
        }

        link.onIdentified = { [weak self] peerId, link in
            guard let self else { return false }
            self.dialing.remove(peerId)
            guard self.byPeer[peerId] == nil else { return false } // already linked
            self.byPeer[peerId] = link
            self.emitPeers()
            return true
        }
        link.onClosed = { [weak self] link in
            guard let self else { return }
            let oid = ObjectIdentifier(link)
            self.allLinks.removeValue(forKey: oid)
            if let expected = self.dialingByLink.removeValue(forKey: oid) {
                self.dialing.remove(expected)
            }
            if let peerId = link.remotePeerId, self.byPeer[peerId] === link {
                self.byPeer.removeValue(forKey: peerId)
                self.emitPeers()
            }
        }
        link.start()
    }

    private func emitPeers() {
        let peers = byPeer.keys.sorted().map(ConnectedPeer.init(id:))
        onPeersChanged?(peers)
    }
}
