import SwiftUI

/// Where the note detail lives for a given horizontal size class. Pure and
/// unit-testable: the acceptance boundary is that compact width pushes detail
/// (stack navigation) while regular width keeps a persistent sidebar.
enum KiemNavigationPolicy {
    case sidebarAndDetail   // regular width: sidebar + detail column
    case stackedDetail       // compact width: push detail onto the stack

    static func policy(for widthClass: UserInterfaceSizeClass?) -> KiemNavigationPolicy {
        switch widthClass {
        case .regular: .sidebarAndDetail
        default: .stackedDetail  // nil (e.g. previews) and .compact both stack
        }
    }

    /// Whether the sidebar column is visible for a horizon vertical size class.
    static func showsSidebar(for widthClass: UserInterfaceSizeClass?) -> Bool {
        policy(for: widthClass) == .sidebarAndDetail
    }
}
