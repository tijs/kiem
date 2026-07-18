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
                        count: model.filterCounts[filter],
                        onDropNotes: dropAction(for: filter)
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
                            count: Int(entry.count),
                            onDropNotes: { model.addTag($0, tag: entry.tag) }
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
                            count: Int(entry.count),
                            onDropNotes: { model.addTag($0, tag: entry.tag) }
                        )
                        .tag(SidebarSelection.tag(entry.tag))
                    }
                }
            }
        }
        .listStyle(.sidebar)
    }

    /// Drop behavior per smart filter — the drag mirror of the right-click
    /// menu: Trash trashes, Pinned pins; the query-shaped filters (Todo,
    /// Today, Untagged) take no drops.
    private func dropAction(for filter: SmartFilter) -> ((Set<String>) -> Void)? {
        switch filter {
        case .trash: { model.trashNotes($0) }
        case .pinned: { model.setPinned($0, pinned: true) }
        case .todo, .today, .untagged: nil
        }
    }
}

/// A sidebar row: an icon-labelled title with an optional trailing count (hidden
/// when zero so empty filters stay quiet). With `onDropNotes` set, the row
/// accepts note drags from the list (payload: newline-joined note ids) and
/// highlights while targeted.
private struct SidebarRow: View {
    let title: String
    let systemImage: String
    let count: Int?
    var onDropNotes: ((Set<String>) -> Void)?

    @State private var isDropTargeted = false

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
        .dropDestination(for: String.self) { payloads, _ in
            guard let onDropNotes else { return false }
            let ids = Set(payloads.flatMap { $0.split(separator: "\n").map(String.init) })
            guard !ids.isEmpty else { return false }
            onDropNotes(ids)
            return true
        } isTargeted: { targeted in
            isDropTargeted = targeted && onDropNotes != nil
        }
        .background(
            isDropTargeted ? Color.accentColor.opacity(0.25) : Color.clear,
            in: RoundedRectangle(cornerRadius: 5)
        )
    }
}
