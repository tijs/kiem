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

    var body: some View {
        if model.selectedNoteID == nil {
            ContentUnavailableView(
                "No note selected",
                systemImage: "square.and.pencil",
                description: Text("Select a note from the list, or create one with ⌘N.")
            )
        } else {
            PulpEditorView(text: $model.editorText)
                .id(model.selectedNoteID) // fresh editor per note
                .onChange(of: model.editorText) {
                    model.editorTextDidChange()
                }
        }
    }
}
