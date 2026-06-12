import SwiftUI

/// Tag navigation. Smart filters (Todo/Today/Untagged/Pinned/Trash) join
/// this sidebar in U12; for now: All Notes + the tag list with counts.
struct SidebarView: View {
    @Bindable var model: KiemModel

    var body: some View {
        List(selection: $model.selectedTag) {
            Label("All Notes", systemImage: "note.text")
                .tag(String?.none)

            if !model.tags.isEmpty {
                Section("Tags") {
                    ForEach(model.tags, id: \.tag) { entry in
                        HStack {
                            Label(entry.tag, systemImage: "number")
                            Spacer()
                            Text("\(entry.count)")
                                .foregroundStyle(.secondary)
                                .monospacedDigit()
                        }
                        .tag(String?.some(entry.tag))
                    }
                }
            }
        }
        .listStyle(.sidebar)
    }
}
