import SwiftUI

/// Sidebar navigation: All Notes, the built-in smart filters, and the tag list
/// with counts. Selecting any row drives `KiemModel.refreshNotes()`.
struct SidebarView: View {
    @Bindable var model: KiemModel

    var body: some View {
        // `List` single-selection wants an optional binding; the model's
        // selection is never absent (All Notes is a real value), so map nil
        // back to `.allNotes`.
        let selection = Binding<SidebarSelection?>(
            get: { model.selection },
            set: { model.selection = $0 ?? .allNotes }
        )

        List(selection: selection) {
            Label("All Notes", systemImage: "note.text")
                .tag(SidebarSelection.allNotes)

            Section("Filters") {
                ForEach(SmartFilter.allCases) { filter in
                    SidebarRow(
                        title: filter.title,
                        systemImage: filter.systemImage,
                        count: model.filterCounts[filter]
                    )
                    .tag(SidebarSelection.filter(filter))
                    .contextMenu {
                        if filter == .trash {
                            Button("Empty Trash…", role: .destructive) {
                                model.isConfirmingEmptyTrash = true
                            }
                        }
                    }
                }
            }

            if !model.projects.isEmpty {
                Section("Projects") {
                    ForEach(model.projects, id: \.tag) { entry in
                        SidebarRow(
                            title: KiemModel.projectName(entry.tag),
                            systemImage: "folder",
                            count: Int(entry.count)
                        )
                        .tag(SidebarSelection.project(entry.tag))
                        .contextMenu {
                            Button("Delete Project…", role: .destructive) {
                                model.projectAwaitingDeletion = entry.tag
                            }
                        }
                    }
                }
            }

            if !model.tags.isEmpty {
                Section("Tags") {
                    ForEach(model.tags, id: \.tag) { entry in
                        SidebarRow(
                            title: entry.tag,
                            systemImage: "number",
                            count: Int(entry.count)
                        )
                        .tag(SidebarSelection.tag(entry.tag))
                    }
                }
            }
        }
        .listStyle(.sidebar)
    }
}

/// A sidebar row: an icon-labelled title with an optional trailing count. The
/// count is hidden when zero so empty filters stay quiet.
private struct SidebarRow: View {
    let title: String
    let systemImage: String
    let count: Int?

    var body: some View {
        HStack {
            Label(title, systemImage: systemImage)
            Spacer()
            if let count, count > 0 {
                Text("\(count)")
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
        }
    }
}
