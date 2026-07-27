import AppKit
import Foundation
import KiemKit

/// Projects and their todos. A project is the reserved `proj/<slug>` tag, so
/// creating one is creating a note and the slug rules are a parity contract
/// with the Rust core — see `projectTag(for:)`.
extension KiemModel {
    /// Toggle a project todo by its (note, index) address and refresh.
    func toggleProjectTodo(noteID: String, index: UInt32, checked: Bool) {
        // A pending body edit to the same note would clobber the toggle.
        flushPendingEdit()
        perform { try $0.setTodoChecked(noteId: noteID, index: index, checked: checked) } then: { _ in
            self.refresh()
            // If the toggled note is open in the editor, re-sync its text. Otherwise
            // the editor keeps the pre-toggle body and the next keystroke writes it
            // back, silently reverting the checkbox.
            if noteID == self.selectedNoteID { self.loadSelectedNote() }
        }
    }

    /// Rename a project todo by its (note, index) address and refresh.
    /// Same clobber guards as `toggleProjectTodo` (see comments there).
    func updateProjectTodoText(noteID: String, index: UInt32, text: String) {
        flushPendingEdit()
        perform { try $0.setTodoText(noteId: noteID, index: index, text: text) } then: { _ in
            self.refresh()
            if noteID == self.selectedNoteID { self.loadSelectedNote() }
        }
    }

    func createProject(name: String) {
        let tag = Self.projectTag(for: name)
        guard !tag.isEmpty else {
            errorMessage = "Couldn’t make a project name from “\(name)”. Use letters or numbers."
            return
        }
        let body = "# \(name)\n\nProject home.\n\n#\(tag)"
        perform { [authorDid] in try $0.createNote(body: body, authorDid: authorDid) } then: { _ in
            self.refreshSidebar()
            self.selection = .project(tag) // didSet refreshes the notes
        }
    }

    /// `proj/<slug>` from a free-form name. Byte-for-byte mirror of the Rust
    /// `to_tag`/`slugify` in `crates/kiem-core/src/project.rs`, enforced by the
    /// shared `fixtures/project-slug.json` parity contract: strip a leading
    /// `proj/`; lowercase ASCII A–Z only (non-ASCII is dropped, NOT Unicode-folded
    /// — `String.lowercased()` would diverge); keep `[a-z0-9/]`; space/`-`/`_` → a
    /// single `_`; collapse repeats; trim `_`. Empty slug → empty tag.
    static func projectTag(for name: String) -> String {
        let raw = name.hasPrefix(projectTagPrefix) ? String(name.dropFirst(projectTagPrefix.count)) : name
        var slug = ""
        var prevSep = false
        for ch in raw {
            // Mirror Rust's `to_ascii_lowercase`: only A–Z fold; everything else
            // is left as-is and then dropped if non-ASCII.
            let out: Character
            if let byte = ch.asciiValue, (65 ... 90).contains(byte) {
                out = Character(UnicodeScalar(byte + 32))
            } else {
                out = ch
            }
            if let byte = out.asciiValue, (97 ... 122).contains(byte) || (48 ... 57).contains(byte) || out == "/" {
                slug.append(out)
                prevSep = false
            } else if out == " " || out == "-" || out == "_" {
                if !prevSep && !slug.isEmpty {
                    slug.append("_")
                    prevSep = true
                }
            }
        }
        while slug.hasSuffix("_") {
            slug.removeLast()
        }
        return slug.isEmpty ? "" : projectTagPrefix + slug
    }
}
