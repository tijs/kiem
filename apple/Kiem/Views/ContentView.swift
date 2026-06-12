import SwiftUI

/// Three-column layout: sidebar (tags) / note list / editor.
struct ContentView: View {
    @Bindable var model: KiemModel

    var body: some View {
        NavigationSplitView {
            SidebarView(model: model)
                .navigationSplitViewColumnWidth(min: 180, ideal: 220)
        } content: {
            NoteListView(model: model)
                .navigationSplitViewColumnWidth(min: 240, ideal: 300)
        } detail: {
            EditorView(model: model)
        }
        .navigationTitle(model.selectedNote?.title ?? "Kiem")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("New Note", systemImage: "square.and.pencil") {
                    model.createNote()
                }
                .keyboardShortcut("n", modifiers: .command)
            }
        }
        .alert(
            "Something went wrong",
            isPresented: Binding(
                get: { model.errorMessage != nil },
                set: { if !$0 { model.errorMessage = nil } }
            )
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(model.errorMessage ?? "")
        }
    }
}
