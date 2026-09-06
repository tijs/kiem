import CoreImage.CIFilterBuiltins
import SwiftUI
import UIKit

/// Device identity, pairing, and sync status (the iOS counterpart of the Mac
/// Sync settings pane). Uses the existing `KiemStore`/iRoH pairing surface —
/// no second networking path.
///
/// The sheet's lifetime *is* the pairing window: it arms on appear and closes
/// on dismiss, so this device can never be discoverable without the sheet
/// saying so. Trust is reciprocal — pairing one side pairs both — so there is
/// no mode to pick.
struct PairingView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var model: KiemModel

    @State private var ticketInput = ""
    @State private var editingDeviceName = false
    @State private var deviceNameDraft = ""
    /// The peer awaiting an unpair confirmation, if any.
    @State private var peerToForget: String?
    /// Wall-clock snapshot advanced once per second by the countdown task. It
    /// is *observable* state, so touching it re-renders the body and lets the
    /// peer rows relax from "Syncing" to "Connected" once the sync-activity
    /// window passes — `peerStatus` is time-dependent but has no other
    /// observable input to drive that transition.
    @State private var now = Date()
    /// Set momentarily after the user copies the pairing code, driving both an
    /// accessibility announcement and a transient "Code copied" confirmation so
    /// VoiceOver users get non-visual feedback that the copy succeeded.
    @State private var codeCopied = false

    var body: some View {
        NavigationStack {
            Form {
                Section("This device") {
                    thisDeviceRow
                }

                Section("Pair a device") {
                    showCode
                    countdown
                    Text("or")
                        .frame(maxWidth: .infinity)
                        .foregroundStyle(.secondary)
                    addCode
                    if let error = model.errorMessage {
                        HStack(alignment: .firstTextBaseline) {
                            Label(error, systemImage: "exclamationmark.triangle.fill")
                                .font(.callout)
                                .foregroundStyle(.red)
                                .accessibilityIdentifier("pairing-error")
                            Spacer()
                            // Inline dismiss so a pairing/rename/unpair error
                            // can be cleared here instead of lingering and
                            // resurfacing in the NoteList alert after the sheet
                            // closes.
                            Button {
                                model.errorMessage = nil
                            } label: {
                                Label("Dismiss", systemImage: "xmark.circle.fill")
                                    .labelStyle(.iconOnly)
                            }
                            .buttonStyle(.borderless)
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier("pairing-error-dismiss")
                        }
                    }
                }

                Section("Paired peers") {
                    if model.knownPeers.isEmpty {
                        Text("No paired devices yet.")
                            .foregroundStyle(.secondary)
                    }
                    ForEach(model.knownPeers, id: \.self) { peerId in
                        peerRow(peerId, now: now)
                    }
                }

                Section("Sync status") {
                    LabeledContent("Connected peers", value: "\(model.connectedPeers.count) / \(model.knownPeers.count)")
                    if model.connectedPeers.isEmpty {
                        Text("Sync runs while the app is in the foreground.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .navigationTitle("Sync & Pairing")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .onAppear {
            model.armPairingWindow()
        }
        .task {
            // 1 Hz countdown tick. A `.task` is automatically cancelled when
            // the view disappears, so no timer can outlive the sheet (the old
            // `Timer.scheduledTimer` leak one-per-appearance). Advancing `now`
            // here is also what frees the peer rows to re-render — see the
            // `now` property docs.
            while !Task.isCancelled {
                model.refreshPairingWindow()
                now = Date()
                try? await Task.sleep(for: .milliseconds(1_000))
            }
        }
        .onDisappear {
            // Deny any still-pending incoming request before closing the
            // window. The alert binding's setter would do the same on
            // dismissal, but SwiftUI isn't guaranteed to write it during sheet
            // teardown — routing the decision through the model here is the
            // safe default so the blocked sync thread can never be orphaned.
            model.resolvePairing(false)
            model.closePairingWindow()
            // Any error surface (rename/unpair/add failure) is scoped to this
            // sheet; clear it on the way out so it can't linger and resurface
            // in the underlying list alert after the sheet closes.
            model.errorMessage = nil
        }
        // Paired successfully — on either side, since trust is reciprocal. A
        // new peer lands in `knownPeers` when we add the other's ticket *or*
        // when an incoming approval succeeds, so the sheet closes itself the
        // moment either direction pairs (same as the Mac sheet).
        .onChange(of: model.knownPeers.count) { old, new in
            if new > old {
                // Success — the ticket (if any) is no longer needed and any
                // error from a previous attempt is stale. The sheet is about
                // to dismiss itself anyway; clearing keeps state clean.
                ticketInput = ""
                dismiss()
            }
        }
        // Let the transient "Code copied" confirmation linger only briefly,
        // then drop it so it doesn't look like a persistent state.
        .task(id: codeCopied) {
            guard codeCopied else { return }
            try? await Task.sleep(for: .seconds(2.5))
            codeCopied = false
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        // Unpairing is destructive enough to confirm — it can only be undone
        // by pairing again, which needs the other device in hand.
        .confirmationDialog(
            "Forget this device?",
            isPresented: Binding(
                get: { peerToForget != nil },
                set: { if !$0 { peerToForget = nil } }
            ),
            presenting: peerToForget
        ) { peerId in
            Button("Forget", role: .destructive) {
                model.forgetDevice(peerId: peerId)
                peerToForget = nil
            }
            Button("Cancel", role: .cancel) { peerToForget = nil }
        } message: { peerId in
            forgetMessage(peerId)
        }
        // An incoming device is asking to pair — the sync thread is blocked on
        // this answer. Attached to the presented sheet (not the note list
        // beneath it) so the prompt is visible above the Sync & Pairing sheet.
        // Dismissing without choosing denies (safe default).
        .alert(
            "Pair this device?",
            isPresented: Binding(
                get: { model.pairingRequest != nil },
                set: { if !$0 { model.resolvePairing(false) } }
            )
        ) {
            Button("Allow") { model.resolvePairing(true) }
            Button("Deny", role: .cancel) { model.resolvePairing(false) }
        } message: {
            Text(model.pairingMessage)
        }
    }

    // MARK: This device

    @ViewBuilder private var thisDeviceRow: some View {
        if editingDeviceName {
            TextField("Device name", text: $deviceNameDraft)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("device-name")
            HStack {
                Button("Save") {
                    // Keep the field (and the editable draft) open if the
                    // rename fails so the user can correct it rather than lose
                    // what they typed.
                    if model.setDeviceName(deviceNameDraft) {
                        editingDeviceName = false
                    }
                }
                .disabled(deviceNameDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                Button("Cancel") { editingDeviceName = false }
            }
        } else {
            HStack {
                Label(model.deviceName.isEmpty ? "This iPhone" : model.deviceName, systemImage: "iphone")
                Spacer()
                Button("Rename") {
                    deviceNameDraft = model.deviceName
                    editingDeviceName = true
                }
                .accessibilityIdentifier("rename-device")
            }
        }
        LabeledContent("ID", value: model.shortDeviceId)
        Text("This device's identity is persisted in the app sandbox and survives relaunch.")
            .font(.caption)
            .foregroundStyle(.secondary)
    }

    // MARK: This device's code (QR + copy/share)

    @ViewBuilder private var showCode: some View {
        // The ticket is only presentable while the pairing window is active.
        // Guarding on `pairingWindowIsActive` (remaining > 0) closes the brief
        // race where the model clears the ticket and the countdown ticks to 0
        // in different frames, so an expired code can never stay scannable.
        if let ticket = model.pairingTicket, model.pairingWindowIsActive {
            if let qr = Self.qrImage(from: ticket) {
                Image(uiImage: qr)
                    .resizable()
                    .interpolation(.none)
                    .frame(width: 200, height: 200)
                    .padding(8)
                    .background(.white)
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                    .frame(maxWidth: .infinity)
                    .accessibilityLabel("QR code to pair this device")
            }
            HStack {
                Spacer()
                Text(model.shortDeviceId + "…")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                Spacer()
            }
            HStack(spacing: 12) {
                Spacer()
                Button {
                    UIPasteboard.general.string = ticket
                    codeCopied = true
                    // Non-visual confirmation for VoiceOver users; the secret
                    // itself is never spoken or logged.
                    UIAccessibility.post(
                        notification: .announcement,
                        argument: "Pairing code copied"
                    )
                } label: {
                    Label("Copy code", systemImage: "doc.on.doc")
                }
                .accessibilityIdentifier("copy-code")
                ShareLink(item: ticket) {
                    Label("Share", systemImage: "square.and.arrow.up")
                }
                .accessibilityIdentifier("share-code")
                if codeCopied {
                    Text("Code copied")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .transition(.opacity)
                }
                Spacer()
            }
            .animation(.default, value: codeCopied)
            Text("Scan this code on your other device — or copy it and paste it there.")
                .font(.caption)
                .foregroundStyle(.secondary)
        } else {
            ProgressView()
                .frame(maxWidth: .infinity)
        }
    }

    @ViewBuilder private var countdown: some View {
        if let remaining = model.pairingWindowRemaining, remaining > 0 {
            Text("Discoverable for \(KiemModel.mmss(remaining))")
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
        } else {
            Button("Make discoverable again") { model.armPairingWindow() }
        }
    }

    // MARK: The other device's code

    private var addCode: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Paste the code shown on your other device.")
                .font(.callout)
                .foregroundStyle(.secondary)
            TextField("Paste a pairing ticket", text: $ticketInput, axis: .vertical)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .lineLimit(3...6)
                .textFieldStyle(.roundedBorder)
                .font(.body.monospaced())
                .accessibilityIdentifier("pairing-code")
            Button("Add device") {
                // Don't clear the ticket here: addDevice is async, so on an
                // invalid ticket the user would have to re-paste it. The input
                // is preserved until (and only cleared upon) success — the
                // knownPeers-count auto-dismiss in the body handles that.
                model.addDevice(ticket: ticketInput)
            }
            .disabled(ticketInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .accessibilityIdentifier("add-device")
        }
    }

    // MARK: Paired peers

    private func peerRow(_ peerId: String, now: Date) -> some View {
        let status = model.peerStatus(for: peerId, now: now)
        return HStack(alignment: .center) {
            VStack(alignment: .leading, spacing: 2) {
                Text(model.peerName(for: peerId))
                    .font(.body)
                    .lineLimit(1)
                    .truncationMode(.middle)
                // The name is peer-supplied, so it's a label; the id identifies.
                Text(String(peerId.prefix(12)) + "…")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                HStack(spacing: 5) {
                    Text(statusText(status))
                        .font(.caption)
                        .foregroundStyle(statusColor(status))
                        .accessibilityIdentifier("peer-status")
                    if status == .syncing {
                        ProgressView()
                            .controlSize(.mini)
                    }
                }
            }
            Spacer()
            Button("Unpair") { peerToForget = peerId }
                .tint(.red)
                .buttonStyle(.bordered)
                .accessibilityIdentifier("unpair-button")
        }
        .swipeActions {
            Button("Unpair", role: .destructive) { peerToForget = peerId }
        }
    }

    @ViewBuilder private func forgetMessage(_ peerId: String) -> some View {
        Text("“\(model.peerName(for: peerId))” will stop syncing with this iPhone. Notes it already sent stay here. To sync with it again you'll have to pair it again.")
    }

    // MARK: Helpers

    private func statusText(_ status: KiemModel.PeerStatus) -> String {
        switch status {
        case .offline: return "Offline"
        case .connected: return "Connected"
        case .syncing: return "Syncing"
        }
    }

    private func statusColor(_ status: KiemModel.PeerStatus) -> Color {
        switch status {
        case .offline: return .secondary
        case .connected: return .green
        case .syncing: return .blue
        }
    }

    /// Renders a ticket string as a QR code via CoreImage's built-in generator
    /// (no dependency). `.interpolation(.none)` upscaling keeps modules crisp.
    private static func qrImage(from string: String) -> UIImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(string.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else { return nil }
        let context = CIContext()
        guard let cgImage = context.createCGImage(output, from: output.extent) else { return nil }
        return UIImage(cgImage: cgImage)
    }
}