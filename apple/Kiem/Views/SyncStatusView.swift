import SwiftUI

/// Quiet sync-status indicator for the toolbar (U13).
///
/// Deliberately invisible for the un-paired single-device user — sync status
/// is noise until there is a peer. With peers it shows connected/total and a
/// tooltip naming the connected devices by short id.
struct SyncStatusView: View {
    let model: KiemModel

    var body: some View {
        if !model.knownPeers.isEmpty {
            let connected = model.connectedPeers.count
            let known = model.knownPeers.count
            Label(
                "\(connected)/\(known)",
                systemImage: connected > 0
                    ? "dot.radiowaves.left.and.right"
                    : "antenna.radiowaves.left.and.right.slash"
            )
            .labelStyle(.titleAndIcon)
            .font(.caption)
            .foregroundStyle(connected > 0 ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
            .help(helpText(connected: connected, known: known))
            .accessibilityLabel("Sync: \(connected) of \(known) paired devices connected")
        }
    }

    private func helpText(connected: Int, known: Int) -> String {
        guard connected > 0 else {
            return "No sync peers reachable (\(known) paired)"
        }
        let ids = model.connectedPeers.map { String($0.prefix(8)) }.joined(separator: ", ")
        return "Syncing with \(connected) of \(known) paired devices: \(ids)"
    }
}
