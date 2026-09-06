import SwiftUI

/// Adaptive iOS shell. `NavigationSplitView` is used unconditionally: on
/// regular width (iPad landscape / large windows) it keeps a persistent
/// sidebar + detail column; on compact width (iPhone) iOS collapses it to a
/// NavigationStack where selecting a row pushes the editor and a leading
/// toolbar button reveals the sidebar. The size-class policy is the pure,
/// unit-tested function in `NavigationPolicy.swift`; the split view implements
/// that same rule natively. The detail (NoteListView) owns the action toolbar,
/// the sync sheet, and the pairing/error alert so they render on both size
/// classes.
struct RootView: View {
    @Bindable var model: KiemModel

    var body: some View {
        NavigationSplitView {
            SidebarView(model: model)
                .navigationSplitViewColumnWidth(min: 220, ideal: 260)
        } detail: {
            NavigationStack {
                NoteListView(model: model)
            }
        }
    }
}
