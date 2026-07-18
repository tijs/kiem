import KiemKit
import Pulp
import SwiftUI

/// The Pulp inline-Markdown editor over the selected note.
///
/// While a note is open, Pulp's text storage is the source of truth; every
/// change flows to the Rust core (which re-derives title/tags) and back into
/// the list metadata. The Automerge-grade bridge (cursor-preserving remote
/// edits during live sync) arrives with U10.
struct EditorView: View {
    @Bindable var model: KiemModel

    /// The handle the formatting bar uses to drive Pulp. Held here so it stays
    /// stable across the per-note editor rebuilds (`.id(...)`); the view re-points
    /// it at the new editor on each rebuild.
    @StateObject private var editorController = PulpEditorController()

    var body: some View {
        if model.selectedNoteIDs.count > 1 {
            ContentUnavailableView(
                "\(model.selectedNoteIDs.count) notes selected",
                systemImage: "square.on.square",
                description: Text("Right-click the list, or drag to a project, tag, Pinned, or Trash — actions apply to all of them.")
            )
        } else if model.selectedNoteID == nil {
            ContentUnavailableView(
                "No note selected",
                systemImage: "square.and.pencil",
                description: Text("Select a note from the list, or create one with ⌘N.")
            )
        } else {
            PulpEditorView(text: $model.editorText, controller: editorController)
                .id(model.selectedNoteID) // fresh editor per note
                .onChange(of: model.editorText) {
                    model.editorTextDidChange()
                }
                // A bottom safe-area inset reliably composites above the AppKit
                // text view (a plain `.overlay` can be drawn under it). The strip
                // is transparent, so the centered pill still reads as floating.
                .safeAreaInset(edge: .bottom, spacing: 0) {
                    FormattingToolbar(controller: editorController)
                        .frame(maxWidth: .infinity)
                        .padding(.bottom, 14)
                }
                .safeAreaInset(edge: .top, spacing: 0) {
                    if let note = model.selectedNote {
                        MetadataStrip(note: note) { model.selection = .project($0) }
                    }
                }
        }
    }
}

/// Reserved-tag (`proj/<slug>`) and, when present, frontmatter-status summary
/// for the open note — additive chrome, not a change to how Pulp renders the
/// body (see the reserved-tag brainstorm: hiding the tag from the rendered
/// body itself would need real TextKit-level work, out of scope here).
private struct MetadataStrip: View {
    let note: NoteMetadata
    let openProject: (String) -> Void

    private var projectTags: [String] {
        note.tags.filter { $0.hasPrefix(KiemModel.projectTagPrefix) }
    }

    var body: some View {
        // Project breadcrumb only. Status is rendered by Pulp's frontmatter
        // callout in the document, so showing it here too double-displays it.
        // Quiet inline row — no bar background, so it reads as a breadcrumb,
        // not chrome competing with the toolbar or the document.
        if !projectTags.isEmpty {
            HStack(spacing: 5) {
                ForEach(projectTags, id: \.self) { tag in
                    Button {
                        openProject(tag)
                    } label: {
                        Label(KiemModel.projectName(tag), systemImage: "folder")
                    }
                    .buttonStyle(.plain)
                    .help("Open project")
                }
                Spacer()
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            .padding(.horizontal, 16)
            .padding(.top, 6)
            .padding(.bottom, 2)
        }
    }
}
