import AppKit
import CoreImage.CIFilterBuiltins
import SwiftUI

/// The whole pairing task on one screen: this Mac's code (QR + copyable string)
/// *and* the field to paste another device's code. Trust is reciprocal — one
/// connection pairs both sides — so there is no mode to pick.
///
/// The sheet's lifetime *is* the pairing window: it arms on appear and closes
/// on dismiss, so this Mac can never be discoverable without the sheet saying so.
struct PairingSheet: View {
    @Bindable var model: KiemModel
    @Environment(\.dismiss) private var dismiss
    @State private var pastedTicket = ""

    private let tick = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    var body: some View {
        VStack(spacing: 16) {
            header
            Divider()
            showCode
            Text("or")
                .font(.callout)
                .foregroundStyle(.secondary)
            addCode
            HStack {
                Spacer()
                Button("Done") { dismiss() }
            }
        }
        .padding(24)
        .frame(width: 380)
        .onAppear { model.armPairingWindow() }
        .onDisappear { model.closePairingWindow() }
        .onReceive(tick) { _ in model.refreshPairingWindow() }
        // Pairing succeeded — on either side, since trust is reciprocal.
        .onChange(of: model.knownPeers.count) { old, new in
            if new > old { dismiss() }
        }
    }

    private var header: some View {
        VStack(spacing: 4) {
            Text("Pair a New Device")
                .font(.headline)
            countdown
        }
    }

    @ViewBuilder private var countdown: some View {
        if let remaining = model.pairingWindowRemaining, remaining > 0 {
            Text("Discoverable for \(Self.mmss(remaining))")
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
        } else {
            Button("Make discoverable again") { model.armPairingWindow() }
        }
    }

    // MARK: This Mac's code

    private var showCode: some View {
        VStack(spacing: 10) {
            Text("Scan this code on your other device — or copy it and paste it there.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)

            if let ticket = model.pairingTicket, let qr = Self.qrImage(from: ticket) {
                Image(nsImage: qr)
                    .resizable()
                    .interpolation(.none)
                    .frame(width: 200, height: 200)
                    .padding(10)
                    .background(.white)
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                // The other device's prompt shows this id — something to compare against.
                Text("This Mac: \(model.shortDeviceId)…")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
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
        }
        .frame(maxWidth: .infinity)
    }

    // MARK: The other device's code

    private var addCode: some View {
        VStack(spacing: 10) {
            Text("Paste the code shown on your other device.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)

            TextField("Paste code…", text: $pastedTicket, axis: .vertical)
                .font(.body.monospaced())
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
