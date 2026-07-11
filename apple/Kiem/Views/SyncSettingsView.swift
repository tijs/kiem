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

    private let tick = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    enum Mode {
        case show, add
    }

    var body: some View {
        Form {
            Section("Status") {
                statusRow
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
        .frame(width: 440, height: 520)
        .onAppear { model.armPairingWindow() }
        .onDisappear { model.closePairingWindow() }
        .onReceive(tick) { _ in
            if mode == .show { model.refreshPairingWindow() }
        }
    }

    // MARK: Status

    @ViewBuilder private var statusRow: some View {
        let known = model.knownPeers.count
        let connected = model.connectedPeers.count
        if known == 0 {
            Label("No devices paired yet", systemImage: "point.3.connected.trianglepath.dotted")
                .foregroundStyle(.secondary)
        } else {
            Label(
                "\(connected) of \(known) paired device\(known == 1 ? "" : "s") connected",
                systemImage: connected > 0 ? "checkmark.circle.fill" : "circle.dashed"
            )
            .foregroundStyle(connected > 0 ? Color.green : Color.secondary)
        }
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
                .lineLimit(3, reservesSpace: true)
                .textFieldStyle(.roundedBorder)

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
