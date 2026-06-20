import SwiftUI
import Pulp

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
        if model.selectedNoteID == nil {
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
        }
    }
}
