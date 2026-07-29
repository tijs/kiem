import AppKit
import SwiftUI

/// The Sync pane of the app's Settings window — where you pair devices (a rare,
/// deliberate action, so it lives here rather than in the main toolbar). Merely
/// opening the pane arms nothing: pairing starts explicitly, in `PairingSheet`,
/// whose lifetime is the pairing window.
struct SyncSettingsView: View {
    @Bindable var model: KiemModel
    @State private var showingPairingSheet = false
    @State private var editingDeviceName = ""
    @State private var isEditingDeviceName = false
    /// The peer awaiting an unpair confirmation, if any.
    @State private var peerToForget: String?
    /// Ticked every second so `peerRow` re-renders and `isSyncing` relaxes to
    /// "Connected" once `syncingTimeout` has passed — nothing else about the
    /// peer state changes purely from time passing.
    @State private var now = Date()

    private let tick = Timer.publish(every: 1, on: .main, in: .common).autoconnect()
    /// How long a peer stays in the "syncing" state after the last activity.
    private static let syncingTimeout: TimeInterval = 2

    var body: some View {
        ScrollView {
            Form {
                Section("This device") {
                    thisDeviceRow
                }

                Section {
                    Button("Pair a New Device…") { showingPairingSheet = true }
                }

                Section("Paired devices") {
                    peerList
                }
            }
            .formStyle(.grouped)
            .frame(minWidth: 440, minHeight: 360)
        }
        .sheet(isPresented: $showingPairingSheet) {
            PairingSheet(model: model)
        }
        .confirmationDialog(
            "Forget this device?",
            isPresented: Binding(get: { peerToForget != nil }, set: { if !$0 { peerToForget = nil } }),
            presenting: peerToForget
        ) { peerId in
            Button("Forget Device", role: .destructive) {
                model.forgetDevice(peerId: peerId)
                peerToForget = nil
            }
            Button("Cancel", role: .cancel) { peerToForget = nil }
        } message: { _ in
            forgetConfirmation
        }
        .onReceive(tick) { date in now = date }
    }

    // MARK: This device

    @ViewBuilder private var thisDeviceRow: some View {
        if isEditingDeviceName {
            HStack {
                TextField("Device name", text: $editingDeviceName)
                    .textFieldStyle(.roundedBorder)
                Button("Save") {
                    model.setDeviceName(editingDeviceName)
                    isEditingDeviceName = false
                }
                .disabled(editingDeviceName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                Button("Cancel") {
                    isEditingDeviceName = false
                }
            }
        } else {
            HStack {
                Label(model.deviceName.isEmpty ? "This Mac" : model.deviceName, systemImage: "laptopcomputer")
                Spacer()
                Button("Rename") {
                    editingDeviceName = model.deviceName
                    isEditingDeviceName = true
                }
            }
        }
    }

    // MARK: Peer list

    @ViewBuilder private var peerList: some View {
        let known = model.knownPeers
        if known.isEmpty {
            Label("No devices paired yet", systemImage: "point.3.connected.trianglepath.dotted")
                .foregroundStyle(.secondary)
        } else {
            ForEach(known, id: \.self) { peerId in
                peerRow(peerId: peerId)
            }
        }
    }

    private func peerRow(peerId: String) -> some View {
        let connected = model.connectedPeers.contains(peerId)
        let syncing = isSyncing(peerId: peerId)
        let name = model.peerName(for: peerId)
        let subtitle = connected
            ? (syncing ? "Syncing" : "Connected")
            : "Offline"

        return HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(name)
                    .font(.body)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(name)
                // The name is peer-supplied, so it's a label; the id identifies.
                Text(peerId.prefix(12) + "…")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                Text(subtitle)
                    .font(.caption)
                    .foregroundStyle(connected ? (syncing ? .blue : .green) : .secondary)
            }
            Spacer()
            if syncing {
                ProgressView()
                    .controlSize(.small)
                    .frame(width: 16, height: 16)
            } else {
                Image(systemName: connected ? "checkmark.circle.fill" : "circle.dashed")
                    .foregroundStyle(connected ? .green : .secondary)
            }
            Button("Forget") { peerToForget = peerId }
                .help("Stop syncing with this device")
                .accessibilityIdentifier("forget-device")
        }
        .padding(.vertical, 2)
    }

    /// Unpairing is destructive enough to confirm — it can only be undone by
    /// pairing again, which needs the other device in hand.
    @ViewBuilder private var forgetConfirmation: some View {
        let name = peerToForget.map { model.peerName(for: $0) } ?? ""
        Text("“\(name)” will stop syncing with this Mac. Notes it already sent stay here. "
            + "To sync with it again you'll have to pair it again.")
    }

    private func isSyncing(peerId: String) -> Bool {
        guard let last = model.lastSyncActivity[peerId] else { return false }
        return now.timeIntervalSince(last) < Self.syncingTimeout
    }
}
