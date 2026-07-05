import Pulp
import SwiftUI

/// A floating formatting bar that lives over the bottom of the editor. Kiem owns
/// this chrome; every control drives Pulp through the shared
/// `PulpEditorController`, so the editor library stays neutral and the actual
/// text mutation lives in Pulp (`PulpFormattingAction`).
///
/// The bar operates on the editor's current selection, which the text view
/// retains even while this overlay holds the pointer — so a button press applies
/// to whatever the caret last touched.
struct FormattingToolbar: View {
    let controller: PulpEditorController

    var body: some View {
        HStack(spacing: 2) {
            headersMenu
            IconButton(symbol: "checklist", help: "Task list") { perform(.taskList) }
            listsMenu

            separator

            IconButton(symbol: "bold", help: "Bold") { perform(.bold) }
            IconButton(symbol: "italic", help: "Italic") { perform(.italic) }
            IconButton(symbol: "strikethrough", help: "Strikethrough") { perform(.strikethrough) }

            separator

            IconButton(symbol: "link", help: "Link") { perform(.link) }
            tableMenu
            overflowMenu
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 5)
        .background {
            Capsule(style: .continuous)
                .fill(.regularMaterial)
                .overlay(
                    Capsule(style: .continuous)
                        .strokeBorder(Color.primary.opacity(0.07), lineWidth: 1)
                )
                // Layered, transparent shadows read as depth on any background.
                // One soft shadow reads as a gentle lift, not a heavy slab.
                .shadow(color: .black.opacity(0.10), radius: 5, y: 2)
        }
    }

    // MARK: Menus

    private var headersMenu: some View {
        IconMenu(symbol: "textformat.size", help: "Heading", showsChevron: true) {
            Button("Heading 1") { perform(.heading(1)) }
            Button("Heading 2") { perform(.heading(2)) }
            Button("Heading 3") { perform(.heading(3)) }
        }
    }

    private var listsMenu: some View {
        IconMenu(symbol: "list.bullet", help: "List", showsChevron: true) {
            Button { perform(.bulletList) } label: {
                Label("Bulleted list", systemImage: "list.bullet")
            }
            Button { perform(.numberList) } label: {
                Label("Numbered list", systemImage: "list.number")
            }
        }
    }

    private var tableMenu: some View {
        IconMenu(symbol: "tablecells", help: "Table") {
            Button { perform(.insertTable(rows: 2, columns: 3)) } label: {
                Label("Insert table", systemImage: "tablecells")
            }
            // Row/column edits only make sense — and only do anything — when the
            // caret already sits in a table. The menu rebuilds each time it opens,
            // so this reads the live caret context.
            if controller.isCaretInTable {
                Divider()
                Section("Row") {
                    Button("Insert above") { controller.insertTableRowAbove() }
                    Button("Insert below") { controller.insertTableRowBelow() }
                    Button("Delete", role: .destructive) { controller.deleteTableRow() }
                }
                Section("Column") {
                    Button("Insert left") { controller.insertTableColumnLeft() }
                    Button("Insert right") { controller.insertTableColumnRight() }
                    Button("Delete", role: .destructive) { controller.deleteTableColumn() }
                }
            }
        }
    }

    private var overflowMenu: some View {
        IconMenu(symbol: "ellipsis", help: "More") {
            Button { perform(.highlight) } label: {
                Label("Highlight", systemImage: "highlighter")
            }
            Button { perform(.inlineCode) } label: {
                Label("Inline code", systemImage: "chevron.left.forwardslash.chevron.right")
            }
            Button { perform(.blockquote) } label: {
                Label("Quote", systemImage: "text.quote")
            }
        }
    }

    private var separator: some View {
        Divider().frame(height: 18).padding(.horizontal, 2)
    }

    private func perform(_ action: PulpFormattingAction) {
        controller.perform(action)
    }
}

// MARK: - Controls

/// An icon-only button sized for the floating bar: a comfortable hit target, a
/// soft hover background, and a subtle press scale for tactile feedback.
private struct IconButton: View {
    let symbol: String
    let help: String
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(.primary)
                .frame(width: 30, height: 30)
                .contentShape(Rectangle())
        }
        .buttonStyle(PressableButtonStyle())
        .background(HoverBackground(hovering: hovering))
        .onHover { hovering = $0 }
        .help(help)
    }
}

/// A menu styled to match `IconButton`, with an optional disclosure chevron.
private struct IconMenu<Content: View>: View {
    let symbol: String
    let help: String
    var showsChevron: Bool = false
    @ViewBuilder let content: Content

    @State private var hovering = false

    var body: some View {
        Menu {
            content
        } label: {
            HStack(spacing: 1) {
                Image(systemName: symbol)
                    .font(.system(size: 14, weight: .medium))
                if showsChevron {
                    Image(systemName: "chevron.down")
                        .font(.system(size: 7, weight: .semibold))
                        .foregroundStyle(.secondary)
                }
            }
            .foregroundStyle(.primary)
            .frame(minWidth: 30, minHeight: 30)
            .contentShape(Rectangle())
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .background(HoverBackground(hovering: hovering))
        .onHover { hovering = $0 }
        .help(help)
    }
}

private struct HoverBackground: View {
    let hovering: Bool

    var body: some View {
        RoundedRectangle(cornerRadius: 7, style: .continuous)
            .fill(Color.primary.opacity(hovering ? 0.08 : 0))
            .animation(.easeOut(duration: 0.12), value: hovering)
    }
}

/// Scales to 0.96 while pressed — interruptible, no bounce.
private struct PressableButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .animation(.spring(duration: 0.3, bounce: 0), value: configuration.isPressed)
    }
}
