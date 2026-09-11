import SwiftUI

/// The mesh's honest, display-only connection state. Derived entirely from
/// existing `KiemModel` state via [`SyncOverview`]; never by reading endpoints,
/// tickets, note contents, or credentials.
enum SyncMeshState: Equatable {
    /// The mesh is not running (stopped, or never started).
    case offline
    /// The mesh is running but nothing is actively syncing this instant.
    case idle
    /// The mesh is running and at least one connected peer is syncing now.
    case syncing

    var displayName: String {
        switch self {
        case .offline: "Offline"
        case .idle: "Idle"
        case .syncing: "Syncing"
        }
    }
}

/// Pure, testable summary of the sync mesh's current state for the status
/// screen. Kept as a static derivation (rather than logic inline in the view)
/// so the screen's honesty is a contract, not an accident of SwiftUI code.
struct SyncOverview: Equatable {
    var meshRunning: Bool
    var knownCount: Int
    var connectedCount: Int
    /// Number of *connected* peers actively syncing right now (0 unless the
    /// mesh is running). Drives the syncing/isn't-syncing distinction.
    var syncingCount: Int
    var state: SyncMeshState
    /// Most recent sync activity across all known peers, if any — the honest
    /// what-can-I-say basis for the "last synced" line.
    var mostRecentActivity: Date?

    /// Pure derivation. `now` is threaded in (never read internally) so the
    /// view can drive re-renders from its own observable 1 Hz clock — the same
    /// pattern [`KiemModel.peerStatus(for:now:)`] uses, so a syncing→idle
    /// transition is observable rather than computed once and frozen.
    nonisolated static func derive(
        meshRunning: Bool,
        knownPeers: [String],
        connectedPeers: [String],
        lastActivityByPeer: [String: Date],
        now: Date,
        syncingTimeout: TimeInterval
    ) -> SyncOverview {
        var syncingCount = 0
        var mostRecent: Date?

        if meshRunning {
            for peerId in connectedPeers {
                if let act = lastActivityByPeer[peerId] {
                    if now.timeIntervalSince(act) < syncingTimeout {
                        syncingCount += 1
                    }
                    if mostRecent == nil || act > mostRecent! {
                        mostRecent = act
                    }
                }
            }
        }

        // The "last synced" line reflects the most recent sync anywhere, even a
        // peer that has since dropped off line — so a quiet-but-previously-busy
        // mesh still reports an honest last-sync time while showing Idle.
        for act in lastActivityByPeer.values where mostRecent == nil || act > mostRecent! {
            mostRecent = act
        }

        let state: SyncMeshState
        if !meshRunning {
            state = .offline
        } else if syncingCount > 0 {
            state = .syncing
        } else {
            state = .idle
        }

        return SyncOverview(
            meshRunning: meshRunning,
            knownCount: knownPeers.count,
            connectedCount: connectedPeers.count,
            syncingCount: syncingCount,
            state: state,
            mostRecentActivity: mostRecent
        )
    }
}

/// The general sync/connection status sheet, opened by the note list's Sync
/// button. It is meant to be an honest, at-a-glance status screen — the "is my
/// mesh up, who's paired, are we syncing" home — *not* the pairing form.
///
/// Pairing is a one-time/infrequent task, so it is deliberately gated behind
/// the "Set Up New Device" navigation below. Merely opening this screen never
/// arms a pairing window (no `armPairingWindow` on appear here); only
/// navigating into [`PairingView`] does, which is exactly when the user
/// explicitly asked to pair.
struct SyncStatusView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var model: KiemModel

    /// Observable wall-clock tick advanced once per second so time-dependent
    /// status (syncing→idle transitions, the relative "last synced" line) can
    /// re-render. Never read `Date()` inside a computed view property; the pure
    /// [`SyncOverview.derive`] takes `now` in.
    @State private var now = Date()

    var body: some View {
        NavigationStack {
            Form {
                Section("Connection") {
                    LabeledContent("Mesh", value: model.isSyncRunning ? "Running" : "Not running")
                        .accessibilityIdentifier("sync-mesh-status")
                    LabeledContent("Paired devices", value: "\(overview.knownCount)")
                        .accessibilityIdentifier("sync-known-count")
                    LabeledContent("Connected", value: "\(overview.connectedCount)")
                        .accessibilityIdentifier("sync-connected-count")
                    overallStateRow
                    LabeledContent("Last sync", value: lastSyncText)
                        .accessibilityIdentifier("sync-last-sync")
                }

                Section("Devices") {
                    if model.knownPeers.isEmpty {
                        Text("No paired devices yet.")
                            .foregroundStyle(.secondary)
                    }
                    ForEach(model.knownPeers, id: \.self) { peerId in
                        peerRow(peerId)
                    }
                }

                Section {
                    NavigationLink("Set Up New Device") {
                        PairingView(model: model)
                    }
                    .accessibilityIdentifier("startPairing")
                } footer: {
                    Text("Pairing is a one-time setup; add a new phone or tablet here.")
                }
            }
            .navigationTitle("Sync Status")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                        .accessibilityIdentifier("statusDone")
                }
            }
        }
        .task {
            // 1 Hz clock. Cancelled automatically when the sheet disappears, so
            // no timer can outlive it.
            while !Task.isCancelled {
                now = Date()
                try? await Task.sleep(for: .milliseconds(1_000))
            }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
    }

    private var overview: SyncOverview {
        SyncOverview.derive(
            meshRunning: model.isSyncRunning,
            knownPeers: model.knownPeers,
            connectedPeers: model.connectedPeers,
            lastActivityByPeer: model.lastSyncActivity,
            now: now,
            syncingTimeout: KiemModel.peerSyncingTimeout
        )
    }

    private var lastSyncText: String {
        guard let date = overview.mostRecentActivity else { return "No sync yet" }
        return date.formatted(.relative(presentation: .named))
    }

    private var overallStateRow: some View {
        let icon = switch overview.state {
        case .offline: "xmark.octagon"
        case .idle: "checkmark.circle"
        case .syncing: "arrow.triangle.2.circlepath"
        }
        return Label(overview.state.displayName, systemImage: icon)
            .accessibilityIdentifier("sync-overall-state")
    }

    @ViewBuilder private func peerRow(_ peerId: String) -> some View {
        let status = model.peerStatus(for: peerId, now: now)
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(model.peerName(for: peerId))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(String(peerId.prefix(12)) + "…")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Text(status.displayName)
                .foregroundStyle(status.color)
                .accessibilityIdentifier("sync-peer-status")
            if status == .syncing {
                ProgressView()
                    .controlSize(.mini)
            }
        }
    }
}

private extension KiemModel.PeerStatus {
    var displayName: String {
        switch self {
        case .offline: "Offline"
        case .connected: "Connected"
        case .syncing: "Syncing"
        }
    }

    var color: Color {
        switch self {
        case .offline: .secondary
        case .connected: .green
        case .syncing: .blue
        }
    }
}