import Foundation

/// Installs the bundled `kiem` command-line tool into the user's PATH by
/// symlinking it to `/usr/local/bin/kiem` (the same model VS Code uses for its
/// `code` command). The binary is embedded in the app bundle's Resources at build
/// time (see the "Embed kiem CLI" build phase in apple/project.yml).
enum CLIInstaller {
    /// Where the symlink is created. `/usr/local/bin` is conventionally on PATH;
    /// it may require admin rights to write, which `install()` handles.
    static let destination = "/usr/local/bin/kiem"

    /// The embedded CLI binary, if this build bundled one.
    static var bundledBinary: URL? {
        Bundle.main.url(forResource: "kiem", withExtension: nil)
    }

    /// Install the CLI, returning a human-readable result for display.
    static func install() -> String {
        guard let source = bundledBinary, FileManager.default.isExecutableFile(atPath: source.path) else {
            return "This build doesn’t include the kiem binary. Build the app in Release (or run apple/build-kiemkit.sh’s sibling CLI embed) and try again."
        }
        if linkDirectly(from: source.path, to: destination) {
            return "Installed. `kiem` is now available at \(destination)."
        }
        return linkWithAdmin(from: source.path, to: destination)
    }

    // MARK: - Internals

    /// Attempt the symlink without elevation; succeeds when the target directory
    /// is already writable (common on Intel/Homebrew-prefixed setups).
    private static func linkDirectly(from source: String, to dest: String) -> Bool {
        let fm = FileManager.default
        let dir = (dest as NSString).deletingLastPathComponent
        let dirExists = fm.fileExists(atPath: dir)
        guard dirExists ? fm.isWritableFile(atPath: dir) : parentIsWritable(dir) else { return false }
        do {
            if !dirExists {
                try fm.createDirectory(atPath: dir, withIntermediateDirectories: true)
            }
            try replaceExisting(at: dest)
            try fm.createSymbolicLink(atPath: dest, withDestinationPath: source)
            return true
        } catch {
            return false
        }
    }

    /// Fall back to an admin-elevated shell command via the standard macOS
    /// authorization prompt (TouchID / password). No credential handling here —
    /// the OS owns the prompt.
    private static func linkWithAdmin(from source: String, to dest: String) -> String {
        let dir = (dest as NSString).deletingLastPathComponent
        // Single-quote the paths for the shell; bundle paths don't contain single quotes.
        let command = "mkdir -p '\(dir)' && ln -sf '\(source)' '\(dest)'"
        let script = "do shell script \"\(command)\" with administrator privileges"
        var error: NSDictionary?
        guard let apple = NSAppleScript(source: script) else {
            return "Couldn’t start the installer."
        }
        apple.executeAndReturnError(&error)
        if let error {
            let message = error[NSAppleScript.errorMessage] as? String ?? "unknown error"
            // User-cancelled the auth prompt (osascript error -128).
            if (error[NSAppleScript.errorNumber] as? Int) == -128 {
                return "Installation cancelled."
            }
            return "Install failed: \(message)"
        }
        return "Installed. `kiem` is now available at \(destination)."
    }

    private static func replaceExisting(at path: String) throws {
        let fm = FileManager.default
        if fm.fileExists(atPath: path) || isSymlink(path) {
            try fm.removeItem(atPath: path)
        }
    }

    private static func parentIsWritable(_ dir: String) -> Bool {
        FileManager.default.isWritableFile(atPath: (dir as NSString).deletingLastPathComponent)
    }

    private static func isSymlink(_ path: String) -> Bool {
        let type = try? FileManager.default.attributesOfItem(atPath: path)[.type] as? FileAttributeType
        return type == .typeSymbolicLink
    }
}
