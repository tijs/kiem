import KiemKit
import SwiftUI

struct NoteListView: View {
    @Bindable var model: KiemModel

    /// Human section titles for known note types, in display order. Unknown
    /// non-`note` types get their own section by capitalized name; `note` (the
    /// default) is the catch-all "Notes" bucket, shown last.
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
            if model.notes.isEmpty && model.projectTodos.isEmpty {
                ContentUnavailableView(
                    model.emptyNotesTitle,
                    systemImage: "note.text",
                    description: Text("Create a note with ⌘N.")
                )
            } else {
                List(selection: $model.selectedNoteID) {
                    if model.isViewingProject && !model.projectTodos.isEmpty {
                        Section("Open todos") {
                            ForEach(todoGroups) { group in
                                // Subtle per-source divider: the list still reads
                                // as one flat todo list, just annotated with which
                                // plan/doc each run of todos came from.
                                Text(todoSourceTitle(group.noteId))
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .padding(.top, 4)
                                    .selectionDisabled()
                                ForEach(group.todos, id: \.self) { todo in
                                    ProjectTodoRow(todo: todo) {
                                        model.toggleProjectTodo(
                                            noteID: todo.noteId, index: todo.index, checked: true
                                        )
                                    }
                                }
                            }
                        }
                    }
                    // Under a project, group notes by kind (Plans, Reviews, …);
                    // elsewhere (All Notes, tags, filters) keep a flat list.
                    if model.isViewingProject {
                        ForEach(noteSections) { section in
                            Section(section.title) {
                                ForEach(section.notes, id: \.id) { note in
                                    noteRow(note)
                                }
                            }
                        }
                    } else {
                        ForEach(model.notes, id: \.id) { note in
                            noteRow(note)
                        }
                    }
                }
            }
        }
        .searchable(text: $model.searchText, prompt: "Search notes")
    }

    /// Open todos chunked into runs by source note, preserving store order
    /// (notes by recency, todos in document order — the store already returns
    /// them contiguously per note, so this is a single grouping pass).
    private var todoGroups: [TodoGroup] {
        var groups: [TodoGroup] = []
        for todo in model.projectTodos {
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

    /// Project notes grouped by `noteType`, known kinds first (in `typeTitles`
    /// order), then any unknown kinds by capitalized name.
    private var noteSections: [NoteSection] {
        var byType: [String: [NoteMetadata]] = [:]
        for note in model.notes {
            byType[note.noteType, default: []].append(note)
        }
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

    private func noteRow(_ note: NoteMetadata) -> some View {
        // A project's own note list already implies the project — the tag
        // there is duplicate info, so show status (the more useful
        // at-a-glance signal for a plan) in its place when present. Other
        // views (All Notes, tag filters, smart filters) keep showing tags.
        NoteRow(note: note, showStatusInsteadOfTags: model.isViewingProject)
            .tag(note.id)
            .contextMenu {
                if model.isViewingTrash {
                    Button("Restore") {
                        model.restoreNote(id: note.id)
                    }
                } else {
                    Button("Move to Trash", role: .destructive) {
                        model.deleteNote(id: note.id)
                    }
                }
            }
    }
}

private struct NoteSection: Identifiable {
    let title: String
    let notes: [NoteMetadata]
    var id: String {
        title
    }
}

private struct TodoGroup: Identifiable {
    let noteId: String
    var todos: [ProjectTodo]
    var id: String {
        noteId
    }
}

/// An open project todo, rendered as a tappable row that completes it.
private struct ProjectTodoRow: View {
    let todo: ProjectTodo
    let onComplete: () -> Void

    var body: some View {
        Button(action: onComplete) {
            Label(todo.text.isEmpty ? "(empty todo)" : todo.text, systemImage: "circle")
                .lineLimit(1)
        }
        .buttonStyle(.plain)
        .help("Mark done")
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
                    StatusBadge(status: status)
                } else {
                    // Hide reserved `proj/...` tags — they're structural, not
                    // user labels, and repeating them on every row is visual
                    // noise. The open note's project shows in the editor's
                    // metadata strip instead.
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
        // .iso8601 *parsing* is lenient about fractional seconds: it accepts
        // the store's variable-precision timestamps (.5965Z, .96832Z, …) and
        // plain ones alike. (Verified against real store data — the
        // `.time(includingFractionalSeconds:)` variant parses none of them.)
        guard let date = try? Date(rfc3339, strategy: .iso8601) else { return rfc3339 }
        return date.formatted(.relative(presentation: .named))
    }
}

/// A small status label ("active"/"completed"/whatever the frontmatter says),
/// styled as a subtle capsule so it doesn't compete visually with the title.
/// Shared between the sidebar row and `EditorView`'s metadata strip.
struct StatusBadge: View {
    let status: String

    var body: some View {
        Text(status)
            .font(.caption2.weight(.medium))
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(.tint.opacity(0.15), in: Capsule())
            .foregroundStyle(.tint)
    }
}
