import SwiftUI

/// Sidebar navigation: All Notes, the built-in smart filters, projects, and
/// the tag list, each with counts. Selecting any row drives
/// `KiemModel.refreshNotes()`.
struct SidebarView: View {
    @Bindable var model: KiemModel

    var body: some View {
        List(selection: selectionBinding) {
            Label("All Notes", systemImage: "note.text")
                .tag(SidebarSelection.allNotes)

            Section("Filters") {
                ForEach(SmartFilter.allCases) { filter in
                    SidebarRow(title: filter.title, systemImage: filter.systemImage, count: model.filterCounts[filter])
                        .tag(SidebarSelection.filter(filter))
                }
            }

            if !model.projects.isEmpty {
                Section("Projects") {
                    ForEach(model.projects, id: \.tag) { entry in
                        SidebarRow(title: KiemModel.projectName(entry.tag), systemImage: "folder", count: Int(entry.count))
                            .tag(SidebarSelection.project(entry.tag))
                    }
                }
            }

            if !model.tags.isEmpty {
                Section("Tags") {
                    ForEach(model.tags, id: \.tag) { entry in
                        SidebarRow(title: entry.tag, systemImage: "number", count: Int(entry.count))
                            .tag(SidebarSelection.tag(entry.tag))
                    }
                }
            }
        }
        .navigationTitle("Kiem")
        #if os(iOS)
        .listStyle(.sidebar)
        #endif
    }

    private var selectionBinding: Binding<SidebarSelection?> {
        Binding(
            get: { model.selection },
            set: { model.selection = $0 ?? .allNotes }
        )
    }
}

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
