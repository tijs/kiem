import AppKit
import KiemKit
import SwiftUI

struct NoteListView: View {
    @Bindable var model: KiemModel

    /// The notes a plain-⌫ press asked to trash; non-nil drives the
    /// confirmation dialog (⌘⌫ trashes immediately, no dialog).
    @State private var notesAwaitingTrash: Set<String>?

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
            if model.notes.isEmpty && model.openTodos.isEmpty {
                ContentUnavailableView(
                    model.emptyNotesTitle,
                    systemImage: "note.text",
                    description: Text("Create a note with ⌘N.")
                )
            } else {
                List(selection: $model.selectedNoteIDs) {
                    if !model.openTodos.isEmpty {
                        Section("Open todos") {
                            ForEach(todoGroups) { group in
                                // Subtle per-source divider: the list still reads
                                // as one flat todo list, just annotated with which
                                // plan/doc each run of todos came from. Tapping it
                                // opens that note.
                                Button {
                                    model.selectedNoteID = group.noteId
                                } label: {
                                    Text(todoSourceTitle(group.noteId))
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                                .buttonStyle(.plain)
                                .padding(.top, 4)
                                .help("Open note")
                                .selectionDisabled()
                                ForEach(group.todos, id: \.self) { todo in
                                    ProjectTodoRow(todo: todo) {
                                        model.toggleProjectTodo(
                                            noteID: todo.noteId, index: todo.index, checked: true
                                        )
                                    } onRename: { text in
                                        model.updateProjectTodoText(
                                            noteID: todo.noteId, index: todo.index, text: text
                                        )
                                    }
                                }
                            }
                        }
                    }
                    // Under the Todo filter the grouped todos above *are* the
                    // list (the captions already name every note), so note rows
                    // only render elsewhere: grouped by kind under a project,
                    // flat for All Notes / tags / the other filters.
                    if model.isViewingTodoFilter {
                        // no note rows
                    } else if model.isViewingProject {
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
                // Right-click acts on the whole selection (or just the row
                // under the cursor when it isn't part of it) — the same
                // actions as dragging notes onto sidebar targets.
                .contextMenu(forSelectionType: String.self) { ids in
                    selectionMenu(ids)
                }
                // Plain ⌫ rides the responder chain (`onDeleteCommand`), so it
                // only fires when the list is first responder — the editor keeps
                // ⌫ = delete-char. ⌘⌫ never reaches this selector (Command
                // re-routes the key), so it's caught by the monitor below.
                .onDeleteCommand {
                    guard !model.selectedNoteIDs.isEmpty, !model.isViewingTrash else { return }
                    notesAwaitingTrash = model.selectedNoteIDs
                }
                // ⌘⌫ = trash the selection instantly. A window-local key
                // monitor is the only thing that reliably sees it (onKeyPress
                // needs SwiftUI focus a row-click doesn't grant; onDeleteCommand
                // never gets the Command-modified key). It yields to any text
                // editor, where ⌘⌫ legitimately means delete-to-line-start.
                .background(CommandDeleteMonitor {
                    guard !model.isViewingTrash, !model.selectedNoteIDs.isEmpty else { return false }
                    model.trashNotes(model.selectedNoteIDs)
                    return true
                })
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if model.isViewingTrash && !model.notes.isEmpty {
                HStack {
                    Spacer()
                    Button("Empty Trash…", role: .destructive) {
                        model.isConfirmingEmptyTrash = true
                    }
                    Spacer()
                }
                .padding(.vertical, 8)
                .background(.bar)
            }
        }
        .searchable(text: $model.searchText, prompt: "Search notes")
        .confirmationDialog(
            "Move to Trash?",
            isPresented: Binding(
                get: { notesAwaitingTrash != nil },
                set: { if !$0 { notesAwaitingTrash = nil } }
            ),
            titleVisibility: .visible,
            presenting: notesAwaitingTrash
        ) { ids in
            Button(trashButtonTitle(ids), role: .destructive) {
                model.trashNotes(ids)
            }
        } message: { _ in
            Text("You can restore from Trash. Tip: ⌘⌫ trashes without asking.")
        }
    }

    private func trashButtonTitle(_ ids: Set<String>) -> String {
        if ids.count == 1, let note = model.notes.first(where: { ids.contains($0.id) }) {
            return "Move “\(note.title.isEmpty ? "Untitled" : note.title)” to Trash"
        }
        return "Move \(ids.count) Notes to Trash"
    }

    /// The right-click menu for a selection — mirror of the sidebar drag
    /// targets: pin (drag to Pinned), add to project (drag to a project),
    /// trash (drag to Trash); restore inside the Trash view.
    @ViewBuilder
    private func selectionMenu(_ ids: Set<String>) -> some View {
        if ids.isEmpty {
            EmptyView()
        } else if model.isViewingTrash {
            Button(ids.count == 1 ? "Restore" : "Restore \(ids.count) Notes") {
                model.restoreNotes(ids)
            }
        } else {
            let selected = model.notes.filter { ids.contains($0.id) }
            let allPinned = !selected.isEmpty && selected.allSatisfy(\.pinned)
            Button(allPinned ? "Unpin" : "Pin") {
                model.setPinned(ids, pinned: !allPinned)
            }
            if !model.projects.isEmpty {
                Menu("Add to Project") {
                    ForEach(model.projects, id: \.tag) { entry in
                        Button(KiemModel.projectName(entry.tag)) {
                            model.addTag(ids, tag: entry.tag)
                        }
                    }
                }
            }
            Divider()
            Button(
                ids.count == 1 ? "Move to Trash" : "Move \(ids.count) Notes to Trash",
                role: .destructive
            ) {
                model.trashNotes(ids)
            }
        }
    }

    /// Open todos chunked into runs by source note, preserving store order
    /// (notes by recency, todos in document order — the store already returns
    /// them contiguously per note, so this is a single grouping pass).
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
            .draggable(dragPayload(for: note))
    }

    /// Drag payload: newline-joined note ids. Dragging a selected row drags
    /// the whole selection; dragging an unselected row drags just that note.
    /// The sidebar's drop targets split this back into a set.
    private func dragPayload(for note: NoteMetadata) -> String {
        let ids = model.selectedNoteIDs.contains(note.id) ? model.selectedNoteIDs : [note.id]
        return ids.sorted().joined(separator: "\n")
    }
}

private struct NoteSection: Identifiable {
    let title: String
    let notes: [NoteMetadata]
    var id: String {
        title
    }
}

/// Installs a window-local ⌘⌫ (Delete key) monitor. `handle` runs the delete
/// and returns whether it consumed the event; returning true swallows the
/// keystroke, false lets it fall through. Yields to text editors so ⌘⌫ keeps
/// its delete-to-line-start meaning while typing.
private struct CommandDeleteMonitor: NSViewRepresentable {
    /// Virtual key code for the Delete/Backspace key.
    private static let deleteKeyCode: UInt16 = 51

    let handle: () -> Bool

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        context.coordinator.monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            guard event.keyCode == Self.deleteKeyCode,
                  event.modifierFlags.contains(.command)
            else { return event }
            // A text view (the editor, or a field editor) owns ⌘⌫ itself.
            if view.window?.firstResponder is NSText { return event }
            return handle() ? nil : event
        }
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {}

    static func dismantleNSView(_ nsView: NSView, coordinator: Coordinator) {
        if let monitor = coordinator.monitor {
            NSEvent.removeMonitor(monitor)
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    final class Coordinator {
        var monitor: Any?
    }
}

private struct TodoGroup: Identifiable {
    let noteId: String
    var todos: [ProjectTodo]
    var id: String {
        noteId
    }
}

/// An open project todo. Only the circle checkbox completes it; clicking the
/// text edits it in place (Return or click-away commits, Escape cancels).
private struct ProjectTodoRow: View {
    let todo: ProjectTodo
    let onComplete: () -> Void
    let onRename: (String) -> Void

    @State private var isEditing = false
    @State private var draft = ""
    /// True once focus was actually granted this edit. Only then does focus
    /// loss mean click-away-commit — a failed macOS focus grant (flaky for a
    /// conditionally-inserted field in a List) must not cancel edit mode.
    @State private var sawFocus = false
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: 6) {
            Button(action: onComplete) {
                Image(systemName: "circle")
            }
            .buttonStyle(.plain)
            .help("Mark done")
            if isEditing {
                TextField("Todo", text: $draft)
                    .textFieldStyle(.plain)
                    .focused($focused)
                    .onAppear { focused = true }
                    .onSubmit(commit)
                    .onExitCommand { isEditing = false }
                    .onChange(of: focused) { _, nowFocused in
                        if nowFocused {
                            sawFocus = true
                        } else if sawFocus {
                            commit()
                        }
                    }
            } else {
                Text(todo.text.isEmpty ? "(empty todo)" : todo.text)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                    .onTapGesture {
                        draft = todo.text
                        sawFocus = false
                        isEditing = true
                    }
                    .help("Edit")
            }
        }
    }

    /// Idempotent: Return fires `onSubmit` and then the focus-loss `onChange`;
    /// the second call sees `isEditing == false` and no-ops. Escape flips
    /// `isEditing` off first, so its focus-loss call cancels the same way.
    private func commit() {
        guard isEditing else { return }
        isEditing = false
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, text != todo.text else { return }
        onRename(text)
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
