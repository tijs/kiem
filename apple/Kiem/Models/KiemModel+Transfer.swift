import Foundation
import KiemKit

/// Markdown import/export (the File menu), run off the main actor with live
/// progress. `activeTransfer` and `transferMessage` live on the class in
/// `KiemModel.swift`; extensions cannot add stored properties.
extension KiemModel {
    /// Export every project's notes as Markdown files under `dir` (one folder
    /// per project), off the main actor with live progress.
    func exportNotes(to dir: URL) {
        runTransfer(verb: "Exporting", work: { [store] relay in
            try store.exportNotes(dir: dir.path, progress: relay)
        }, summary: { summary in
            var text = "Exported \(summary.transferred) notes to “\(dir.lastPathComponent)”."
            if summary.skipped > 0 {
                text += " Skipped \(summary.skipped) notes that aren’t in a project."
            }
            return text
        })
    }

    /// Import a folder of Markdown files as notes, off the main actor with
    /// live progress. `foldersAsProjects` maps folders to projects (subfolders
    /// each, or the flat folder itself); without it notes keep only the tags
    /// already in their bodies (e.g. a Bear/Obsidian dump).
    func importNotes(from dir: URL, foldersAsProjects: Bool) {
        runTransfer(verb: "Importing", work: { [store, authorDid] relay in
            try store.importNotes(
                dir: dir.path,
                authorDid: authorDid,
                foldersAsProjects: foldersAsProjects,
                progress: relay
            )
        }, summary: { summary in
            var text = "Imported \(summary.transferred) notes from “\(dir.lastPathComponent)”."
            if summary.skipped > 0 {
                text += " \(summary.skipped) were already present and were skipped."
            }
            return text
        })
    }

    /// One transfer at a time: run `work` on the store queue (it holds the
    /// store+sync lock — sync rounds stall until it finishes, by design),
    /// stream progress into `activeTransfer`, then refresh and report the
    /// summary or error back on the main actor.
    func runTransfer(
        verb: String,
        work: @escaping @Sendable (TransferProgressRelay) throws -> TransferSummary,
        summary: @escaping @Sendable (TransferSummary) -> String
    ) {
        guard activeTransfer == nil else { return }
        // The transfer holds the store lock for its whole run. Flushing first
        // puts the pending write ahead of it on the store queue — the modal
        // progress sheet prevents new edits until the transfer ends.
        flushPendingEdit()
        activeTransfer = TransferActivity(verb: verb)
        let relay = TransferProgressRelay { [weak self] done, total in
            Task { @MainActor in
                guard var transfer = self?.activeTransfer else { return }
                // Independent task hops aren't FIFO — drop a late, smaller
                // update instead of walking the bar backwards.
                guard done > transfer.done || total != transfer.total else { return }
                transfer.done = done
                transfer.total = total
                self?.activeTransfer = transfer
            }
        }
        storeQueue.async {
            let result = Result { try work(relay) }
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    switch result {
                    case let .success(transferred):
                        self.refresh()
                        self.pendingTransferOutcome = (summary(transferred), isError: false)
                    case let .failure(error):
                        self.pendingTransferOutcome = ("\(error)", isError: true)
                    }
                    // Dropping activeTransfer dismisses the progress sheet; the
                    // outcome alert waits for the sheet's onDismiss — presenting
                    // it while the sheet is still tearing down can silently drop
                    // it on macOS.
                    self.activeTransfer = nil
                }
            }
        }
    }

    /// Called from the progress sheet's `onDismiss`: surface the held outcome.
    func transferSheetDismissed() {
        guard let (message, isError) = pendingTransferOutcome else { return }
        pendingTransferOutcome = nil
        if isError {
            errorMessage = message
        } else {
            transferMessage = message
        }
    }
}

/// Forwards Rust transfer progress (delivered on the transfer's background
/// thread) to the main actor. Holds only a @Sendable closure.
final class TransferProgressRelay: TransferProgress, @unchecked Sendable {
    private let update: @Sendable (Int, Int) -> Void

    init(update: @escaping @Sendable (Int, Int) -> Void) {
        self.update = update
    }

    func onProgress(done: UInt32, total: UInt32) {
        update(Int(done), Int(total))
    }
}
