import Foundation
import KiemKit

/// The platform-neutral query surface over `KiemStore`: maps a sidebar
/// selection (or a search) to the exact note list the Rust core returns. This
/// is the seam unit tests drive against real `KiemKit`/Rust behavior — the
/// filtering itself lives in the core, so a test failure here means the Swift
/// mapping drifted from a store query, not that the query is wrong.
///
/// Static and opaque over the store so it stays testable without an
/// `@MainActor` model.
enum StoreQuery {
    /// The note list backing a sidebar selection. Each case maps to a dedicated
    /// `KiemStore` query.
    static func notes(for selection: SidebarSelection, in store: KiemStore) throws -> [NoteMetadata] {
        switch selection {
        case .allNotes: try store.listNotes()
        case let .tag(tag): try store.listByTag(tag: tag)
        case let .project(tag): try store.listByTag(tag: tag)
        case .filter(.todo): try store.listTodos()
        case .filter(.today): try store.listToday()
        case .filter(.untagged): try store.listUntagged()
        case .filter(.pinned): try store.listPinned()
        case .filter(.trash): try store.listDeleted()
        }
    }

    /// Full-text search via the Rust core, mapped back to list metadata with
    /// rank order preserved. Trashed hits drop out — they're not in `listNotes`.
    static func searchResults(matching query: String, in store: KiemStore) throws -> [NoteMetadata] {
        let hits = try store.search(query: query, limit: 50)
        let byID = try Dictionary(uniqueKeysWithValues: store.listNotes().map { ($0.id, $0) })
        return hits.compactMap { byID[$0.noteId] }
    }

    /// Sidebar smart-filter counts + tag list, in one store scan so the counts
    /// can't disagree with the tags.
    static func sidebarSnapshot(store: KiemStore) throws -> SidebarSnapshot {
        let counts = try store.filterCounts()
        let tags = try store.getTags()
        return SidebarSnapshot(
            projects: tags.filter { $0.tag.hasPrefix(KiemModel.projectTagPrefix) },
            tags: tags.filter { !$0.tag.hasPrefix(KiemModel.projectTagPrefix) },
            filterCounts: [
                .todo: Int(counts.todo),
                .today: Int(counts.today),
                .untagged: Int(counts.untagged),
                .pinned: Int(counts.pinned),
                .trash: Int(counts.trash),
            ]
        )
    }
}

/// Immutable sidebar data pulled from the store in one call — keeps the Swift
/// model's stored-property writes trivial and the mapping unit-testable.
struct SidebarSnapshot {
    let projects: [TagCount]
    let tags: [TagCount]
    let filterCounts: [SmartFilter: Int]
}
