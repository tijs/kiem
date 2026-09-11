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
                    // Return-to-active: release any bounded background pairing
                    // session, re-arm the mesh if it was paused, and resume
                    // polling. `startSync` is idempotent, so the mesh that was
                    // kept discoverable through the background just continues.
                    model?.handleSceneReturnedToForeground()
                case .inactive, .background:
                    // Persist any debounce-pending edit, pause polling, then stop
                    // the foreground sync mesh — UNLESS a pairing window is open,
                    // in which case the mesh stays discoverable under a bounded
                    // background request so the user can paste the code elsewhere.
                    model?.handleSceneLeavingForeground()
                default:
                    break
                }
            }
        }
    }
}
