import AppKit

/// Opts out of macOS's automatic window/selection state restoration.
///
/// Without this, relaunching the app can silently re-select a previously
/// open note at the AppKit/NSOutlineView level a moment after launch,
/// leaving the editor blank (or showing stale content) over a model that
/// never got told to load it. Kiem is a single-window app with its own
/// explicit state (`KiemModel`); there's no reason to let the OS restore
/// anything underneath it.
///
/// `applicationSupportsSecureRestorableState` returning `false` does **not**
/// disable restoration — confirmed directly via `log show`: with only that
/// override in place, relaunching still logged
/// `-[NSApplication _reopenWindowsAsNecessaryIncludingRestorableState:...]
/// shouldRestoreState=1 hasPersistentStateToRestore=1`, followed by an actual
/// `restoreWindowWithIdentifier:`. That flag only selects which *encoding*
/// macOS uses for the saved-state archive (secure vs. legacy); it was never a
/// switch to turn restoration off. The actual opt-out is marking the window
/// itself non-restorable, which is what `isRestorable = false` below does.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
        false
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        for window in NSApplication.shared.windows {
            window.isRestorable = false
        }
    }
}
