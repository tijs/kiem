import SwiftUI

@main
struct KiemApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var model: KiemModel?
    @State private var startupError: String?
    @State private var cliInstallMessage: String?

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
        }
        .commands {
            CommandGroup(after: .appInfo) {
                Button("Install Command Line Tool…") {
                    cliInstallMessage = CLIInstaller.install()
                }
            }
        }
    }

    private func start() {
        do {
            model = try KiemModel(dataDir: KiemModel.defaultDataDir())
        } catch {
            startupError = "\(error)"
        }
    }
}
