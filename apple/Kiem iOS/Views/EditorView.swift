import SwiftUI

/// The note editor. For this first usable slice it is a focused native SwiftUI
/// editor that edits the exact Markdown body and reuses Pulp's platform-neutral
/// `ContentAnalyzer` to derive the live title/tags preview ("content-derivation
/// parity" with what the Rust core re-derives at flush time).
///
/// LIMITATION (recorded, not hidden): this is a plain Markdown-source editor,
/// not a rich-rendering one. It does NOT have the full macOS Pulp rich-render
/// parity (GFM table overlays, checkbox/attachment drawing, code-block
/// backgrounds, marker-hiding). A full UIKit/TextKit 2 Pulp port is a follow-on
/// slice; this fallback keeps the first usable app shippable and honest.
struct EditorView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var model: KiemModel
    let noteID: String

    @FocusState private var focused: Bool

    private var bodyBinding: Binding<String> {
        Binding(
            get: { model.editorText },
            set: { model.editorText = $0; model.editorTextDidChange() }
        )
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            TextEditor(text: bodyBinding)
                .focused($focused)
                .font(.body.monospaced())
                .autocorrectionDisabled()
                #if os(iOS)
                .textInputAutocapitalization(.never)
                #endif
                .padding(8)
        }
        .navigationTitle(derivedTitle.isEmpty ? "Untitled" : derivedTitle)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItemGroup(placement: .bottomBar) {
                Button {
                    toggleCheckboxOnCurrentLine()
                } label: {
                    Image(systemName: "checklist")
                }
                .accessibilityLabel("Toggle todo checkbox")
                .accessibilityIdentifier("toggleTodoButton")
                Spacer()
                Button {
                    model.setPinned(noteID, pinned: !(model.selectedNote?.pinned ?? false))
                } label: {
                    Image(systemName: (model.selectedNote?.pinned ?? false) ? "pin.fill" : "pin")
                }
                .accessibilityLabel((model.selectedNote?.pinned ?? false) ? "Unpin note" : "Pin note")
                .accessibilityIdentifier("pinButton")
                Spacer()
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
        .onAppear {
            model.selectedNoteID = noteID
            focused = true
        }
        .onDisappear {
            model.flushPendingEdit()
        }
    }

    private var derivedTitle: String {
        KiemModel.derive(titleFrom: model.editorText)
    }

    private var derivedTags: [String] {
        KiemModel.derive(tagsFrom: model.editorText)
    }

    private var hasUncheckedTodos: Bool {
        KiemModel.derive(hasUncheckedTodosFrom: model.editorText)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(derivedTitle.isEmpty ? "Untitled" : derivedTitle)
                .font(.title3.weight(.semibold))
                .textSelection(.enabled)
            if !derivedTags.isEmpty {
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 60), spacing: 6, alignment: .leading)],
                    alignment: .leading,
                    spacing: 6
                ) {
                    ForEach(derivedTags, id: \.self) { tag in
                        Text("#\(tag)")
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(.tint.opacity(0.12), in: Capsule())
                            .foregroundStyle(.tint)
                            .font(.caption)
                    }
                }
            }
            HStack {
                Text(hasUncheckedTodos ? "Has open todos" : "No open todos")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Spacer()
                if let version = model.loadedVersion {
                    Text("v\(version.prefix(8))")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
            }
            if model.rejectedEditorDraft != nil {
                Label("This note changed elsewhere. Your stale edit wasn't applied; the latest body was reloaded.", systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
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
        // caret tracking is beyond the fallback editor's scope).
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
