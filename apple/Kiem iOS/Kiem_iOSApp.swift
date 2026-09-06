import SwiftUI

@main
struct Kiem_iOSApp: App {
    /// The Rust-backed store, opened once for the app's lifetime.
    @State private var model: KiemModel?
    @State private var modelError: String?
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            Group {
                if let model {
                    RootView(model: model)
                } else if let modelError {
                    ContentUnavailableView(
                        "Couldn't open the note store",
                        systemImage: "exclamationmark.triangle",
                        description: Text(modelError)
                    )
                } else {
                    ProgressView("Opening Kiem store…")
                }
            }
            .task {
                if model == nil && modelError == nil {
                    do {
                        model = try KiemModel(dataDir: DataDirectory.resolve())
                    } catch {
                        modelError = "\(error)"
                    }
                }
            }
            .onChange(of: scenePhase) { _, phase in
                switch phase {
                case .active:
                    // Return-to-active: re-arm the sync mesh paused when the
                    // scene left the foreground, and resume polling. Both are
                    // idempotent, so repeated `.active` events are safe.
                    model?.startSync()
                    model?.beginForegroundPolling()
                case .inactive, .background:
                    // Persist any debounce-pending edit before the process can
                    // be suspended, and stop the foreground sync mesh safely.
                    model?.flushPendingEditBlocking()
                    model?.pauseForegroundPolling()
                    model?.stopSync()
                default:
                    break
                }
            }
        }
    }
}
