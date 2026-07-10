import SwiftUI

/// Three-column layout: sidebar (tags) / note list / editor.
struct ContentView: View {
    @Bindable var model: KiemModel
    @State private var showingNewProject = false
    @State private var newProjectName = ""

    var body: some View {
        NavigationSplitView {
            SidebarView(model: model)
                .navigationSplitViewColumnWidth(min: 180, ideal: 200)
        } content: {
            NoteListView(model: model)
                .navigationSplitViewColumnWidth(min: 240, ideal: 340)
        } detail: {
            EditorView(model: model)
        }
        // Static app name, not the selected note's title — the title is already
        // the H1 at the top of the editor and in the note-list row, so showing it
        // again in the window title bar is redundant chrome.
        .navigationTitle("Kiem")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("New Project", systemImage: "folder.badge.plus") {
                    newProjectName = ""
                    showingNewProject = true
                }
                .keyboardShortcut("n", modifiers: [.command, .shift])
            }
            ToolbarItem(placement: .primaryAction) {
                Button("New Note", systemImage: "square.and.pencil") {
                    model.createNote()
                }
                .keyboardShortcut("n", modifiers: .command)
            }
        }
        .alert("New Project", isPresented: $showingNewProject) {
            TextField("Project name", text: $newProjectName)
            Button("Create") {
                let name = newProjectName.trimmingCharacters(in: .whitespacesAndNewlines)
                if !name.isEmpty { model.createProject(name: name) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Creates a project. To let an agent work in a repo, run “kiem project add” there.")
        }
        // Shared by the trash list's button and the sidebar's context menu.
        .confirmationDialog(
            "Permanently erase all notes in the Trash?",
            isPresented: $model.isConfirmingEmptyTrash,
            titleVisibility: .visible
        ) {
            Button("Empty Trash", role: .destructive) {
                model.emptyTrash()
            }
        } message: {
            Text("This can’t be undone.")
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
