import SwiftUI
import KiemKit

/// The note list for the current selection, with note-type grouping under
/// project views (matching the macOS list) and empty states. Rows push the
/// editor via a `NavigationLink(value:)`, so compact width stacks the editor
/// on top while regular width keeps the split sidebar.
struct NoteListView: View {
    @Bindable var model: KiemModel

    @State private var showSync = false

    private static let typeTitles: [(type: String, title: String)] = [
        ("brainstorm", "Brainstorms"),
        ("plan", "Plans"),
        ("review", "Reviews"),
        ("decision", "Decisions"),
        ("solution", "Solutions"),
        ("doc", "Docs"),
        ("note", "Notes"),
    ]

    var body: some View {
        Group {
            if model.notes.isEmpty && model.openTodos.isEmpty {
                ContentUnavailableView(
                    model.emptyNotesTitle,
                    systemImage: "note.text",
                    description: Text("Tap + to create a note.")
                )
            } else {
                List {
                    if !model.openTodos.isEmpty {
                        Section("Open todos") {
                            ForEach(todoGroups) { group in
                                Button {
                                    model.selectedNoteID = group.noteId
                                } label: {
                                    Text(todoSourceTitle(group.noteId))
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                                ForEach(group.todos, id: \.self) { todo in
                                    ProjectTodoRow(todo: todo) {
                                        model.setTodoChecked(noteID: todo.noteId, index: todo.index, checked: true)
                                    }
                                }
                            }
                        }
                    }
                    if model.isViewingTodoFilter {
                        // Open-todo groups above are the view.
                    } else if model.isViewingProject {
                        ForEach(noteSections) { section in
                            Section(section.title) {
                                ForEach(section.notes, id: \.id) { note in
                                    noteLink(note)
                                }
                            }
                        }
                    } else {
                        ForEach(model.notes, id: \.id) { note in
                            noteLink(note)
                        }
                    }
                }
            }
        }
        .navigationTitle(title)
        .searchable(text: $model.searchText, prompt: "Search notes")
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button { showSync = true } label: {
                    Label("Sync", systemImage: model.connectedPeers.isEmpty ? "arrow.triangle.2.circlepath" : "arrow.triangle.2.circlepath.fill")
                }
                .accessibilityIdentifier("syncButton")
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button(action: { model.createNote() }) {
                    Label("New note", systemImage: "square.and.pencil")
                }
                .accessibilityIdentifier("composeButton")
            }
            if model.isViewingTrash && !model.notes.isEmpty {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Empty Trash", role: .destructive) { model.emptyTrash() }
                }
            }
        }
        .sheet(isPresented: $showSync) {
            PairingView(model: model)
        }
        // Non-pairing error alert stays on the list. The pairing-request alert
        // lives on the presented PairingView so it is visible above the Sync &
        // Pairing sheet; this gated copy only fires when the sheet is closed,
        // so the two can never present simultaneously.
        .alert(
            "Kiem",
            isPresented: Binding(
                get: { !showSync && model.errorMessage != nil },
                set: { if !$0 { model.errorMessage = nil } }
            )
        ) {
            Button("OK", role: .cancel) { model.errorMessage = nil }
        } message: {
            Text(model.errorMessage ?? "")
        }
        .alert(
            "Pair this device?",
            isPresented: Binding(
                get: { !showSync && model.pairingRequest != nil },
                set: { if !$0 { model.resolvePairing(false) } }
            )
        ) {
            Button("Allow") { model.resolvePairing(true) }
            Button("Deny", role: .cancel) { model.resolvePairing(false) }
        } message: {
            Text(model.pairingMessage)
        }
        .navigationDestination(for: String.self) { id in
            EditorView(model: model, noteID: id)
        }
    }

    private var title: String {
        switch model.selection {
        case .allNotes: "All Notes"
        case let .tag(tag): "#\(tag)"
        case let .project(tag): KiemModel.projectName(tag)
        case let .filter(filter): filter.title
        }
    }

    @ViewBuilder
    private func noteLink(_ note: NoteMetadata) -> some View {
        NavigationLink(value: note.id) {
            NoteRow(note: note, showStatusInsteadOfTags: model.isViewingProject)
        }
        .contextMenu {
            if model.isViewingTrash {
                Button("Restore") { model.restoreNote(note.id) }
            } else {
                Button(note.pinned ? "Unpin" : "Pin") { model.setPinned(note.id, pinned: !note.pinned) }
                Button("Trash", role: .destructive) { model.deleteNote(note.id) }
                ForEach(model.projects, id: \.tag) { entry in
                    Button("Add to \(KiemModel.projectName(entry.tag))") {
                        // Re-add only if the note isn't already in the project.
                        if !note.tags.contains(entry.tag) { model.addTag(note.id, tag: entry.tag) }
                    }
                }
            }
        }
    }

    private var todoGroups: [TodoGroup] {
        var groups: [TodoGroup] = []
        for todo in model.openTodos {
            if let last = groups.indices.last, groups[last].noteId == todo.noteId {
                groups[last].todos.append(todo)
            } else {
                groups.append(TodoGroup(noteId: todo.noteId, todos: [todo]))
            }
        }
        return groups
    }

    private func todoSourceTitle(_ noteId: String) -> String {
        let title = model.notes.first { $0.id == noteId }?.title ?? ""
        return title.isEmpty ? "Untitled" : title
    }

    private var noteSections: [NoteSection] {
        var byType: [String: [NoteMetadata]] = [:]
        for note in model.notes { byType[note.noteType, default: []].append(note) }
        var sections: [NoteSection] = []
        var placed: Set<String> = []
        for (type, title) in Self.typeTitles {
            if let notes = byType[type], !notes.isEmpty {
                sections.append(NoteSection(title: title, notes: notes))
                placed.insert(type)
            }
        }
        for type in byType.keys.sorted() where !placed.contains(type) {
            sections.append(NoteSection(title: type.capitalized, notes: byType[type] ?? []))
        }
        return sections
    }
}

private struct NoteSection: Identifiable {
    let title: String
    let notes: [NoteMetadata]
    var id: String { title }
}

private struct TodoGroup: Identifiable {
    let noteId: String
    var todos: [ProjectTodo]
    var id: String { noteId }
}

private struct ProjectTodoRow: View {
    let todo: ProjectTodo
    let onComplete: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Button(action: onComplete) {
                Image(systemName: "circle")
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Mark \\(todo.text.isEmpty ? \"todo item\" : todo.text) complete")
            .accessibilityIdentifier("completeTodoButton")
            Text(todo.text.isEmpty ? "(empty todo)" : todo.text)
                .lineLimit(1)
        }
    }
}

private struct NoteRow: View {
    let note: NoteMetadata
    let showStatusInsteadOfTags: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(note.title.isEmpty ? "Untitled" : note.title)
                .font(.headline)
                .lineLimit(2)
            HStack(spacing: 6) {
                Text(Self.dateText(note.modifiedAt))
                    .foregroundStyle(.secondary)
                if showStatusInsteadOfTags, let status = note.status {
                    Text(status)
                        .font(.caption2.weight(.medium))
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(.tint.opacity(0.15), in: Capsule())
                        .foregroundStyle(.tint)
                } else {
                    ForEach(note.tags.filter { !$0.hasPrefix(KiemModel.projectTagPrefix) }, id: \.self) { tag in
                        Text("#\(tag)")
                            .foregroundStyle(.tint)
                    }
                }
            }
            .font(.caption)
            .lineLimit(1)
        }
        .padding(.vertical, 4)
    }

    private static func dateText(_ rfc3339: String) -> String {
        guard let date = try? Date(rfc3339, strategy: .iso8601) else { return rfc3339 }
        return date.formatted(.relative(presentation: .named))
    }
}
