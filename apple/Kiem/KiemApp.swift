import SwiftUI

@main
struct KiemApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var model: KiemModel?
    @State private var startupError: String?
    @State private var cliInstallMessage: String?
    @State private var cliShadowPath: String?
    private static let cliShadowDismissedKey = "kiem.cliShadowWarningDismissed"

    var body: some Scene {
        WindowGroup {
            Group {
                if let model {
                    ContentView(model: model)
                } else if let startupError {
                    ContentUnavailableView(
                        "Kiem cannot open its data directory",
                        systemImage: "externaldrive.badge.exclamationmark",
                        description: Text(startupError)
                    )
                } else {
                    ProgressView().task { start() }
                }
            }
            .alert(
                "Command Line Tool",
                isPresented: Binding(
                    get: { cliInstallMessage != nil },
                    set: { if !$0 { cliInstallMessage = nil } }
                )
            ) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(cliInstallMessage ?? "")
            }
            .alert(
                "Another kiem CLI is shadowing the app",
                isPresented: Binding(
                    get: { cliShadowPath != nil },
                    set: { if !$0 { cliShadowPath = nil } }
                )
            ) {
                Button("Remove it", role: .destructive) {
                    if let path = cliShadowPath { CLIInstaller.removeShadowing(at: path) }
                    cliShadowPath = nil
                }
                Button("Keep it", role: .cancel) {
                    UserDefaults.standard.set(true, forKey: Self.cliShadowDismissedKey)
                    cliShadowPath = nil
                }
            } message: {
                if let path = cliShadowPath {
                    Text("“\(path)” is on PATH ahead of the app’s CLI, so `kiem` won’t auto-update with the app. Remove it? You can reinstall it later with `cargo install`.")
                }
            }
        }
        .commands {
            // Kiem is a single-window app (see AppDelegate.swift): the stock
            // File > New Window command would open a second NSWindow that
            // defaults to `isRestorable = true`, un-protected by
            // AppDelegate's launch-time-only fix and able to reintroduce the
            // window-restoration bug that fix closed.
            CommandGroup(replacing: .newItem) {}
            CommandGroup(after: .appInfo) {
                Button("Install Command Line Tool…") {
                    cliInstallMessage = CLIInstaller.install()
                }
            }
        }
    }

    private func start() {
        // Keep the PATH symlink pointed at the bundled CLI so `kiem` tracks the
        // installed app version with no user interaction (the VS Code `code`
        // model). Silent + idempotent; never blocks launch.
        let cliInstalled = CLIInstaller.ensureInstalled()
        do {
            model = try KiemModel(dataDir: KiemModel.defaultDataDir())
        } catch {
            startupError = "\(error)"
        }
        // Only offer to clear a shadowing cargo CLI once the bundle symlink is
        // in place — otherwise removing it would leave no `kiem` on PATH.
        if cliInstalled,
           !UserDefaults.standard.bool(forKey: Self.cliShadowDismissedKey),
           let shadow = CLIInstaller.shadowingBinary() {
            cliShadowPath = shadow
        }
    }
}
