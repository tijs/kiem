import SwiftUI

/// The note editor. Unlike the earlier plain-monospace `TextEditor`, it renders
/// inline Markdown using Pulp's cross-platform `MarkdownTokenizer`/`MarkdownStyler`
/// (via `MarkdownEditor`/`MarkdownEditorRenderer`), hiding syntax markers and
/// styling headings/emphasis/code/links/lists so the document reads like the Mac
/// Pulp editor. It edits the exact Markdown source, which rounds-trips through
/// the model's debounced version-aware write.
///
/// Layout: no large duplicate title/header — the document owns the viewport, the
/// inline navigation title carries the note title, and the note controls
/// (todo/pin/delete) live in the top-right toolbar so they stay reachable when
/// the keyboard is up (they are not in a bottom bar the keyboard would cover).
/// A dedicated keyboard toolbar offers a Done action and compact metadata.
/// The editor does not auto-focus on appear; the user taps to edit.
struct EditorView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var model: KiemModel
    let noteID: String

    private var bodyBinding: Binding<String> {
        Binding(
            get: { model.editorText },
            set: { model.editorText = $0 }
        )
    }

    var body: some View {
        MarkdownEditor(text: bodyBinding) {
            model.editorTextDidChange()
        }
        .navigationTitle(derivedTitle.isEmpty ? "Untitled" : derivedTitle)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                Button {
                    toggleCheckboxOnCurrentLine()
                } label: {
                    Image(systemName: "checklist")
                }
                .accessibilityLabel("Toggle todo checkbox")
                .accessibilityIdentifier("toggleTodoButton")

                Button {
                    model.setPinned(noteID, pinned: !(model.selectedNote?.pinned ?? false))
                } label: {
                    Image(systemName: (model.selectedNote?.pinned ?? false) ? "pin.fill" : "pin")
                }
                .accessibilityLabel((model.selectedNote?.pinned ?? false) ? "Unpin note" : "Pin note")
                .accessibilityIdentifier("pinButton")

                Button(role: .destructive) {
                    model.deleteNote(noteID)
                    dismiss()
                } label: {
                    Image(systemName: "trash")
                }
                .accessibilityLabel("Delete note")
                .accessibilityIdentifier("trashButton")
            }
        }
        .overlay(alignment: .top) {
            if model.rejectedEditorDraft != nil {
                Label("This note changed elsewhere. Your stale edit wasn't applied; the latest body was reloaded.", systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .padding(10)
                    .frame(maxWidth: .infinity)
                    .background(.thinMaterial)
            }
        }
        .onAppear {
            // Select (and load) the note — deliberately NOT auto-focusing the
            // editor on appear.
            model.selectedNoteID = noteID
        }
        .onDisappear {
            model.flushPendingEdit()
        }
    }

    private var derivedTitle: String {
        KiemModel.derive(titleFrom: model.editorText)
    }

    private var hasUncheckedTodos: Bool {
        KiemModel.derive(hasUncheckedTodosFrom: model.editorText)
    }

    /// Toggle a todo checkbox on the current line (basic todo editing). Runs
    /// through the normal debounced version-aware write.
    private func toggleCheckboxOnCurrentLine() {
        // Work on the newest loaded store body whenever possible so a toggled
        // checkbox applies to a fresh buffer.
        model.flushPendingEdit()
        // Simple line-based toggle on editorText.
        let lines = model.editorText.split(separator: "\n", omittingEmptySubsequences: false)
        // We only target the first unchecked line for this slice (multi-line
        // caret tracking is beyond this editor's scope).
        if let idx = lines.firstIndex(where: { $0.hasPrefix("- [ ]") }) {
            var copy = lines
            copy[idx] = Substring("- [x]\(lines[idx].dropFirst("- [ ]".count))")
            model.editorText = copy.joined(separator: "\n")
            model.editorTextDidChange()
        } else if !hasUncheckedTodos {
            let prefix = model.editorText.isEmpty ? "" : (model.editorText.hasSuffix("\n") ? "" : "\n")
            model.editorText += "\(prefix)- [ ] "
            model.editorTextDidChange()
        }
    }
}
