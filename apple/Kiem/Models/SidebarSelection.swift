import Foundation

/// A built-in "smart" filter in the sidebar, each backed by a dedicated
/// `KiemStore` query (the filtering logic lives — and is tested — in the Rust
/// core). Distinct from tag navigation.
enum SmartFilter: String, CaseIterable, Identifiable {
    case todo, today, untagged, pinned, trash

    var id: String { rawValue }

    var title: String {
        switch self {
        case .todo: "Todo"
        case .today: "Today"
        case .untagged: "Untagged"
        case .pinned: "Pinned"
        case .trash: "Trash"
        }
    }

    var systemImage: String {
        switch self {
        case .todo: "checklist"
        case .today: "sun.max"
        case .untagged: "tag.slash"
        case .pinned: "pin"
        case .trash: "trash"
        }
    }

    /// Empty-list message shown when this filter matches nothing.
    var emptyTitle: String {
        switch self {
        case .todo: "No unchecked todos"
        case .today: "Nothing modified today"
        case .untagged: "No untagged notes"
        case .pinned: "No pinned notes"
        case .trash: "Trash is empty"
        }
    }
}

/// What the sidebar currently has selected: all notes, a smart filter, or a
/// single tag. Drives `KiemModel.refreshNotes()`. Always has a value — "All
/// Notes" is `.allNotes`, not the absence of a selection.
enum SidebarSelection: Hashable {
    case allNotes
    case filter(SmartFilter)
    case tag(String)
    /// A project, identified by its full reserved tag (`proj/<slug>`).
    case project(String)
}
