import SwiftUI

@main
struct KiemApp: App {
    @State private var model: KiemModel?
    @State private var startupError: String?

    var body: some Scene {
        WindowGroup {
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
    }

    private func start() {
        do {
            model = try KiemModel(dataDir: KiemModel.defaultDataDir())
        } catch {
            startupError = "\(error)"
        }
    }
}
