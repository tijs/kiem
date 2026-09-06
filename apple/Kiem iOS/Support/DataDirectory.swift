import Foundation

/// The device's private note store directory.
///
/// iOS: the app-sandbox Application Support directory (KiemCore survives
/// relaunch, is private to the app, and never collides with another app's).
/// The Rust identity, known-peers and sync files live alongside `kiem.db` in
/// the same directory, exactly as on macOS.
///
/// `KIEM_DATA_DIR` overrides it — the sanctioned hook for iOS UI tests that
/// launch the real app against a throwaway scratch store.
enum DataDirectory {
    static func resolve() -> URL {
        if let override = ProcessInfo.processInfo.environment["KIEM_DATA_DIR"] {
            return URL(fileURLWithPath: override)
        }
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/Application Support")
        let url = base.appendingPathComponent("Kiem", isDirectory: true)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }
}
