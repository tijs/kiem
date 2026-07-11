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
        // An incoming device is asking to pair — the sync thread is blocked on
        // this answer. Dismissing without choosing denies (safe default).
        .confirmationDialog(
            "Pair this device?",
            isPresented: Binding(
                get: { model.pairingRequest != nil },
                set: { if !$0 { model.resolvePairing(false) } }
            ),
            titleVisibility: .visible,
            presenting: model.pairingRequest
        ) { request in
            Button("Allow") { model.resolvePairing(true) }
            Button("Don’t Allow", role: .cancel) { model.resolvePairing(false) }
        } message: { request in
            Text("A device (\(request.shortPeerId)…) wants to pair and sync your notes. Only allow devices you recognize.")
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
        // Raised from the sidebar's project context menu.
        .confirmationDialog(
            "Delete this project?",
            isPresented: Binding(
                get: { model.projectAwaitingDeletion != nil },
                set: { if !$0 { model.projectAwaitingDeletion = nil } }
            ),
            titleVisibility: .visible,
            presenting: model.projectAwaitingDeletion
        ) { tag in
            Button("Delete “\(KiemModel.projectName(tag))” and All Its Notes", role: .destructive) {
                model.deleteProject(tag: tag)
            }
        } message: { _ in
            Text(
                "Permanently erases the project and every note in it — including any in the Trash. This can’t be undone."
            )
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
