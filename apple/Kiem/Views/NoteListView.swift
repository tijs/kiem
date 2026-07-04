import SwiftUI
import KiemKit

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
                            ForEach(model.projectTodos, id: \.self) { todo in
                                ProjectTodoRow(todo: todo) {
                                    model.toggleProjectTodo(
                                        noteID: todo.noteId, index: todo.index, checked: true
                                    )
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

    @ViewBuilder
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
    var id: String { title }
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
                .lineLimit(1)
            HStack(spacing: 6) {
                Text(Self.dateText(note.modifiedAt))
                    .foregroundStyle(.secondary)
                if showStatusInsteadOfTags, let status = note.status {
                    StatusBadge(status: status)
                } else {
                    ForEach(note.tags, id: \.self) { tag in
                        Text("#\(tag)")
                            .foregroundStyle(.tint)
                    }
                }
            }
            .font(.caption)
            .lineLimit(1)
        }
        .padding(.vertical, 2)
    }

    private static func dateText(_ rfc3339: String) -> String {
        guard let date = ISO8601DateFormatter.flexible.date(from: rfc3339) else {
            return rfc3339
        }
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

extension ISO8601DateFormatter {
    /// Note timestamps carry fractional seconds; tolerate both forms.
    static let flexible: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()
}
