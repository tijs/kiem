import AppKit

/// Opts out of macOS's automatic window/selection state restoration.
///
/// Without this, relaunching the app can silently re-select a previously
/// open note at the AppKit/NSOutlineView level a moment after launch —
/// updating `KiemModel.selectedNoteID` correctly (confirmed: the note body
/// does load into `editorText`) but *without* the already-rendered SwiftUI
/// view tree ever redrawing to reflect it, leaving the editor showing
/// "No note selected" over a model that actually has a note loaded. Kiem is
/// a single-window app with its own explicit state (`KiemModel`); there's
/// no reason to let the OS restore anything underneath it.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
        false
    }
}
