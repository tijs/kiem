import AppKit
import XCTest

/// UI regression tests for the flows that burned us and that scripts can't
/// drive (SwiftUI List selection, keyboard shortcuts, quit/relaunch):
///
/// - the blank-editor class of bug: tap a note, the body must render — also
///   checked across a quit/relaunch, which is where the old window-state
///   restoration variant hid.
/// - keyboard deletes: ⌫ asks, ⌘⌫ trashes instantly.
/// - Empty Trash permanently erases.
///
/// Every test runs against its own throwaway `KIEM_DATA_DIR`; the user's
/// real `~/.kiem` is never touched.
@MainActor
final class KiemUITests: XCTestCase {
    private var dataDir: URL!

    override func setUp() {
        continueAfterFailure = false
        dataDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("kiem-uitest-\(UUID().uuidString)")
    }

    override func tearDown() {
        if let dataDir {
            try? FileManager.default.removeItem(at: dataDir)
        }
    }

    private func launchApp() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment["KIEM_DATA_DIR"] = dataDir.path
        app.launch()
        return app
    }

    /// Create a note through the toolbar and give it a body. Clicks into the
    /// editor first — after ⌘N the editor deliberately doesn't steal key
    /// focus, so typing without the click would go to the list.
    private func createNote(_ app: XCUIApplication, body: String) {
        // First launch can take a while to bind sync and open the store, so
        // wait for the toolbar rather than assuming it's up immediately.
        let newNote = app.buttons["New Note"].firstMatch
        XCTAssertTrue(newNote.waitForExistence(timeout: 30), "app did not reach the main window")
        newNote.click()
        let editor = app.textViews.firstMatch
        XCTAssertTrue(editor.waitForExistence(timeout: 5), "editor did not appear after New Note")
        editor.click()
        editor.typeText(body)
    }

    /// The row for a note in the middle list, matched by its title text.
    private func noteRow(_ app: XCUIApplication, title: String) -> XCUIElement {
        app.staticTexts[title].firstMatch
    }

    // MARK: - Tests

    /// The alpha.10 bug class: a selected note must actually render its body,
    /// on first open and again after a full quit/relaunch (the restoration
    /// variant of the same symptom).
    func testNoteBodyRendersAndSurvivesRelaunch() {
        var app = launchApp()
        createNote(app, body: "Relaunch survivor\nbody line one")

        let row = noteRow(app, title: "Relaunch survivor")
        XCTAssertTrue(row.waitForExistence(timeout: 5), "note row did not appear in the list")

        app.terminate()
        app = launchApp()

        let rowAgain = noteRow(app, title: "Relaunch survivor")
        XCTAssertTrue(rowAgain.waitForExistence(timeout: 5), "note row missing after relaunch")
        rowAgain.click()

        let editor = app.textViews.firstMatch
        XCTAssertTrue(editor.waitForExistence(timeout: 5), "editor did not mount after selecting the note")
        let value = editor.value as? String ?? ""
        XCTAssertTrue(
            value.contains("body line one"),
            "editor did not render the note body after relaunch (got: \(value.prefix(80)))"
        )
    }

    /// ⌫ on a selected list row asks for confirmation; ⌘⌫ trashes instantly.
    func testKeyboardDeletes() {
        let app = launchApp()
        createNote(app, body: "Confirm delete me")

        let row = noteRow(app, title: "Confirm delete me")
        XCTAssertTrue(row.waitForExistence(timeout: 5))
        row.click()

        // Plain ⌫ → confirmation dialog names the note; cancel keeps it.
        app.typeKey(.delete, modifierFlags: [])
        let confirmButton = app.buttons["Move “Confirm delete me” to Trash"].firstMatch
        XCTAssertTrue(confirmButton.waitForExistence(timeout: 5), "⌫ did not raise the confirmation dialog")
        app.typeKey(.escape, modifierFlags: [])
        XCTAssertTrue(row.waitForExistence(timeout: 5), "cancelling the dialog should keep the note")

        // ⌘⌫ → gone without any dialog.
        row.click()
        app.typeKey(.delete, modifierFlags: [.command])
        let disappeared = row.waitForNonExistence(timeout: 5)
        XCTAssertTrue(disappeared, "⌘⌫ did not trash the note instantly")
        XCTAssertFalse(
            app.buttons["Move “Confirm delete me” to Trash"].exists,
            "⌘⌫ must not raise a confirmation dialog"
        )
    }

    /// Deleting the open note keeps the editor on the next remaining row.
    func testDeletingSelectedNoteSelectsNextNote() {
        let app = launchApp()
        createNote(app, body: "First remaining note\nbody one")
        createNote(app, body: "Delete this note\nbody two")

        let deletedRow = noteRow(app, title: "Delete this note")
        XCTAssertTrue(deletedRow.waitForExistence(timeout: 5))
        deletedRow.click()
        app.typeKey(.delete, modifierFlags: [.command])
        XCTAssertTrue(deletedRow.waitForNonExistence(timeout: 5))

        let editor = app.textViews.firstMatch
        XCTAssertTrue(editor.waitForExistence(timeout: 5), "next note did not open after deletion")
        let value = editor.value as? String ?? ""
        XCTAssertTrue(value.contains("body one"), "the remaining note was not selected (got: \(value.prefix(80)))")
    }

    /// A code copied from the app must survive the Add-a-device text field and
    /// register a peer (using this app's own code keeps the test local).
    func testCopiedPairingCodeCanBePasted() {
        let app = launchApp()
        XCTAssertTrue(app.buttons["New Note"].firstMatch.waitForExistence(timeout: 30))
        app.typeKey(",", modifierFlags: .command)

        app.buttons["Pair a New Device…"].firstMatch.click()

        let copyCode = app.buttons["Copy code"].firstMatch
        XCTAssertTrue(copyCode.waitForExistence(timeout: 25), "pairing code did not load")
        copyCode.click()
        let copied = NSPasteboard.general.string(forType: .string)
        XCTAssertFalse(copied?.isEmpty ?? true, "Copy code did not write a ticket")

        let input = app.descendants(matching: .any)["pairing-code"]
        XCTAssertTrue(input.waitForExistence(timeout: 5))
        input.click()
        app.typeKey("v", modifierFlags: .command)
        XCTAssertEqual(input.value as? String, copied, "paste changed the pairing code")
        app.buttons["Add device"].firstMatch.click()

        XCTAssertTrue(
            app.staticTexts["No devices paired yet"].waitForNonExistence(timeout: 5),
            "the copied code was rejected"
        )
    }

    /// Pairing lives in Settings (⌘,), behind an explicit button: opening the
    /// pane must NOT make this Mac discoverable, and pressing the button must
    /// show both halves of the handshake on one screen.
    func testPairingStartsOnlyWhenAsked() {
        let app = launchApp()
        XCTAssertTrue(app.buttons["New Note"].firstMatch.waitForExistence(timeout: 30), "app did not reach the main window")

        // Pairing setup must not clutter the main window.
        XCTAssertFalse(app.buttons["Add Device"].exists, "pairing should live in Settings, not the toolbar")

        // Open Settings → Sync pane.
        app.typeKey(",", modifierFlags: .command)

        let startPairing = app.buttons["Pair a New Device…"].firstMatch
        XCTAssertTrue(startPairing.waitForExistence(timeout: 10), "Sync pane did not offer to pair")
        // The pane itself arms nothing — no code is shown until asked.
        XCTAssertFalse(app.buttons["Copy code"].exists, "the pane armed pairing without being asked")

        startPairing.click()

        // "Copy code" appears only after the ticket loads (which waits briefly
        // for a relay hint), so give it room.
        XCTAssertTrue(
            app.buttons["Copy code"].firstMatch.waitForExistence(timeout: 25),
            "the pairing sheet did not show this Mac's code"
        )
        // Both halves live on the one screen — no mode to pick.
        XCTAssertTrue(
            app.buttons["Add device"].firstMatch.exists,
            "the pairing sheet did not offer to add the other device's code"
        )
    }

    /// Empty Trash erases everything in the trash after one confirmation.
    func testEmptyTrash() {
        let app = launchApp()
        createNote(app, body: "Erase me forever")

        let row = noteRow(app, title: "Erase me forever")
        XCTAssertTrue(row.waitForExistence(timeout: 5))
        row.click()
        app.typeKey(.delete, modifierFlags: [.command])
        XCTAssertTrue(row.waitForNonExistence(timeout: 5))

        app.staticTexts["Trash"].firstMatch.click()
        let trashedRow = noteRow(app, title: "Erase me forever")
        XCTAssertTrue(trashedRow.waitForExistence(timeout: 5), "trashed note not listed in Trash")

        app.buttons["Empty Trash…"].firstMatch.click()
        // Scope to the window tree — macOS also mirrors buttons onto the Touch
        // Bar element tree, which firstMatch would otherwise grab (and can't click).
        let confirm = app.windows.buttons["Empty Trash"].firstMatch
        XCTAssertTrue(confirm.waitForExistence(timeout: 5), "Empty Trash did not ask for confirmation")
        confirm.click()

        XCTAssertTrue(trashedRow.waitForNonExistence(timeout: 5), "trash was not emptied")
        XCTAssertTrue(
            app.staticTexts["Trash is empty"].waitForExistence(timeout: 5),
            "empty state did not appear"
        )
    }
}
