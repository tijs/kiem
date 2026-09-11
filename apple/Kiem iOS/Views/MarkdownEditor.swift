import Pulp
import SwiftUI
import UIKit

/// Pure renderer: turns Markdown source into rich attributed text using Pulp's
/// cross-platform `MarkdownTokenizer` + `MarkdownStyler`, hiding syntax markers
/// (shrunk / cleared) and styling headings, emphasis, code, links, and lists —
/// the iOS analogue of the Mac Pulp editor's inline rendering. It never rewrites
/// the source string, so the exact Markdown round-trips through the version-aware
/// write. Kept as a pure static so the regression suite can pin the behavior
/// without a live UIKit view.
enum MarkdownEditorRenderer {
    static func styledText(_ text: String, theme: PulpTheme = .default) -> NSAttributedString {
        let styler = MarkdownStyler(theme: theme)
        let result = NSMutableAttributedString(
            string: text,
            attributes: styler.baseAttributes()
        )
        let tokens = MarkdownTokenizer().tokenize(text)
        for run in styler.styleRuns(for: tokens) {
            let range = run.range
            // Defensive: ignore any run the styler (or tokenizer) maps outside the
            // buffer so styling can never throw or corrupt typing.
            guard range.location >= 0, range.location + range.length <= result.length else { continue }
            result.addAttributes(run.attributes, range: range)
        }
        return result
    }
}

/// An editable rich-text Markdown editor: a `UITextView` that renders Pulp's
/// inline Markdown styling while keeping the exact Markdown source in the
/// binding. Styling is applied as attribute-only updates to the text storage
/// (characters unchanged), so the caret/selection and typed-character undo
/// survive every edit.
struct MarkdownEditor: UIViewRepresentable {
    @Binding var text: String
    /// Called after the user edits, so the model can run its debounced
    /// version-aware write (`model.editorTextDidChange()`).
    var textDidChange: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> UITextView {
        let textView = UITextView()
        textView.delegate = context.coordinator
        textView.accessibilityIdentifier = "note-editor"
        textView.backgroundColor = PulpTheme.default.backgroundColor
        textView.textContainerInset = UIEdgeInsets(top: 12, left: 16, bottom: 12, right: 16)
        // Never auto-focus on appear — the user brings up the keyboard by tapping.
        // Interactive dismissal lets the user pull the keyboard away directly.
        textView.keyboardDismissMode = .interactive
        // A visible, reliable keyboard-dismiss button: unlike a SwiftUI
        // `.keyboard`-placement toolbar (which does not attach to a text view
        // hosted in a UIViewRepresentable), a UIKit inputAccessoryView is
        // guaranteed to appear above the keyboard and is reachable by its
        // `editorDone` accessibility identifier. A shared Done affordance can
        // also come from the SwiftUI `.keyboard` toolbar in EditorView when the
        // system attaches it; this is the always-present one.
        textView.inputAccessoryView = makeKeyboardBar(target: context.coordinator)
        textView.autocorrectionType = .no
        textView.spellCheckingType = .no
        textView.autocapitalizationType = .none
        textView.smartQuotesType = .no
        textView.smartDashesType = .no
        textView.smartInsertDeleteType = .no
        // Fresh type inherits the base body attributes (rich inline styling is
        // re-applied on every edit via `applyStyling`).
        textView.typingAttributes = MarkdownStyler(theme: .default).baseAttributes()
        textView.text = text
        applyStyling(textView)
        return textView
    }

    func updateUIView(_ uiView: UITextView, context: Context) {
        // Only an *external* change (note reload / conflict rejection reloading
        // the latest body) replaces the source; while the user is typing the
        // text view is already the source of truth and must not be jumped.
        guard uiView.text != text else { return }
        uiView.text = text
        applyStyling(uiView)
        // A freshly loaded body should be typed *after* the existing content, not
        // prepended. This handles the note body arriving asynchronously after the
        // editor has already become first responder (when the default start caret
        // would otherwise corrupt the note's first line).
        if text.count > 0 {
            uiView.selectedRange = NSRange(location: text.count, length: 0)
        }
    }

    /// Builds the always-present keyboard accessory bar with a Done button whose
    /// `editorDone` identifier the UI tests (and VoiceOver users) can reach. The
    /// bar is the reliable dismiss affordance for a representable-hosted text
    /// view; a SwiftUI `.keyboard` toolbar is kept alongside for parity.
    private func makeKeyboardBar(target: MarkdownEditor.Coordinator) -> UIToolbar {
        let bar = UIToolbar(frame: CGRect(x: 0, y: 0, width: UIScreen.main.bounds.width, height: 44))
        let done = UIButton(type: .system)
        done.setTitle("Done", for: .normal)
        done.titleLabel?.font = .systemFont(ofSize: 17, weight: .semibold)
        done.addTarget(target, action: #selector(MarkdownEditor.Coordinator.dismissKeyboard), for: .touchUpInside)
        done.accessibilityIdentifier = "editorDone"
        done.isAccessibilityElement = true
        done.sizeToFit()
        bar.items = [
            UIBarButtonItem(barButtonSystemItem: .flexibleSpace, target: nil, action: nil),
            UIBarButtonItem(customView: done),
        ]
        bar.sizeToFit()
        return bar
    }

    /// Apply Pulp's styling as attribute-only edits to the text storage. The
    /// character string is untouched, so the caret/selection stay put and
    /// typed-character undo is preserved. Does not trigger `textViewDidChange`,
    /// so there is no delegate/update recursion.
    private func applyStyling(_ textView: UITextView) {
        guard textView.text.count > 0 else { return }
        let styled = MarkdownEditorRenderer.styledText(textView.text)
        let storage = textView.textStorage
        storage.beginEditing()
        styled.enumerateAttributes(in: NSRange(location: 0, length: storage.length), options: []) { attrs, range, _ in
            guard range.location >= 0, range.location + range.length <= storage.length else { return }
            storage.setAttributes(attrs, range: range)
        }
        storage.endEditing()
        // Let the next typed character inherit the attribute at the caret.
        let caret = textView.selectedRange.location
        if caret >= 0, caret < storage.length {
            textView.typingAttributes = storage.attributes(at: caret, effectiveRange: nil)
        }
    }

    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: MarkdownEditor
        init(_ parent: MarkdownEditor) { self.parent = parent }

        /// Keyboard Done: resign first responder regardless of which responder
        /// currently owns the keyboard.
        @objc func dismissKeyboard() {
            UIApplication.shared.sendAction(
                #selector(UIResponder.resignFirstResponder),
                to: nil, from: nil, for: nil
            )
        }

        func textViewDidChange(_ textView: UITextView) {
            let newText = textView.text ?? ""
            // Push the exact Markdown source into the binding (model.editorText)
            // then let the model run its debounced write-back.
            parent.text = newText
            parent.textDidChange()
            parent.applyStyling(textView)
        }

        /// When the user focuses by tapping, the system may default the caret to
        /// the start (0), so continued typing would prepend to the body. Deferring
        /// one runloop lets the system finish placing the tap caret, then we move
        /// the untouched caret to the end so typing appends (the behaviour users
        /// and the previous `TextEditor` expect). A user who tapped a specific
        /// word keeps their caret.
        func textViewDidBeginEditing(_ textView: UITextView) {
            DispatchQueue.main.async { [weak textView] in
                guard let tv = textView,
                      tv.selectedRange.location == 0,
                      tv.text.count > 0
                else { return }
                tv.selectedRange = NSRange(location: tv.text.count, length: 0)
            }
        }
    }
}