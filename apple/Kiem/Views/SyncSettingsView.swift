import AppKit
import CoreImage.CIFilterBuiltins
import SwiftUI

/// The Sync pane of the app's Settings window — where you pair devices (a rare,
/// deliberate action, so it lives here rather than in the main toolbar). Opening
/// the pane arms a single-use pairing window; closing it closes the window.
/// Auto-reciprocal pairing means one action on either side is enough.
struct SyncSettingsView: View {
    @Bindable var model: KiemModel
    @State private var mode: Mode = .show
    @State private var pastedTicket = ""
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

    enum Mode {
        case show, add
    }

    var body: some View {
        ScrollView {
            Form {
                Section("This device") {
                    thisDeviceRow
                }

                Section("Paired devices") {
                    peerList
                }

                Section("Pair a device") {
                    Picker("", selection: $mode) {
                        Text("Show this Mac").tag(Mode.show)
                        Text("Add a device").tag(Mode.add)
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()

                    switch mode {
                    case .show: showPane
                    case .add: addPane
                    }
                }
            }
            .formStyle(.grouped)
            .frame(minWidth: 440, minHeight: 560)
        }
        .onAppear { model.armPairingWindow() }
        .onDisappear { model.closePairingWindow() }
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
        .onReceive(tick) { date in
            now = date
            if mode == .show { model.refreshPairingWindow() }
        }
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

    // MARK: Show this Mac

    private var showPane: some View {
        VStack(spacing: 12) {
            Text("Scan this code on your other device — or copy it and paste it there.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)

            if let ticket = model.pairingTicket, let qr = Self.qrImage(from: ticket) {
                Image(nsImage: qr)
                    .resizable()
                    .interpolation(.none)
                    .frame(width: 200, height: 200)
                    .padding(10)
                    .background(.white)
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                Button {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(ticket, forType: .string)
                } label: {
                    Label("Copy code", systemImage: "doc.on.doc")
                }
            } else {
                ProgressView()
                    .frame(width: 200, height: 200)
            }

            countdown
        }
        .frame(maxWidth: .infinity)
    }

    @ViewBuilder private var countdown: some View {
        if let remaining = model.pairingWindowRemaining, remaining > 0 {
            Text("Ready to pair · \(Self.mmss(remaining))")
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
        } else {
            Button("Ready again") { model.armPairingWindow() }
        }
    }

    // MARK: Add a device

    private var addPane: some View {
        VStack(spacing: 12) {
            Text("Paste the code shown on your other device.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)

            TextField("Paste code…", text: $pastedTicket, axis: .vertical)
                .font(.body.monospaced())
                .multilineTextAlignment(.leading)
                .lineLimit(4, reservesSpace: true)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("pairing-code")

            Button("Add device") {
                model.addDevice(ticket: pastedTicket)
                pastedTicket = ""
            }
            .keyboardShortcut(.defaultAction)
            .disabled(pastedTicket.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
        .frame(maxWidth: .infinity)
    }

    // MARK: Helpers

    private static func mmss(_ secs: Int) -> String {
        String(format: "%d:%02d", secs / 60, secs % 60)
    }

    /// Renders a ticket string as a QR code via CoreImage's built-in generator
    /// (no dependency). `.interpolation(.none)` upscaling keeps modules crisp.
    private static func qrImage(from string: String) -> NSImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(string.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else { return nil }
        let rep = NSCIImageRep(ciImage: output)
        let image = NSImage(size: rep.size)
        image.addRepresentation(rep)
        return image
    }
}
