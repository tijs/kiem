import XCTest

/// UI tests launch the real Kiem iOS app against a throwaway scratch store
/// (injected via `KIEM_DATA_DIR`), then drive note creation, selection, Markdown
/// editing, and relaunch persistence end-to-end through SwiftUI/UIKit.
final class Kiem_iOSUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    /// Launch the real app against a fresh scratch store; returns (app, dir).
    private func launchApp() -> (app: XCUIApplication, storeDir: URL) {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("kiem-ios-ui-\(UUID().uuidString)", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let app = XCUIApplication()
        app.launchEnvironment["KIEM_DATA_DIR"] = dir.path
        return (app, dir)
    }

    /// Creating a note, opening it, editing Markdown, and popping back —
    /// the typed body must survive a full app relaunch against the same
    /// store (verified by reopening the note and reading the editor text).
    @MainActor
    func testCreateEditAndRelaunchPersistence() throws {
        let (app, _) = launchApp()
        app.launch()

        // Empty store → empty state.
        XCTAssertTrue(app.staticTexts["No notes yet"].waitForExistence(timeout: 20),
                      "expected empty state on a fresh store")

        // Compose a note; it appears in All Notes as an untitled row.
        app.buttons["composeButton"].tap()
        let untitled = app.staticTexts["Untitled"]
        XCTAssertTrue(untitled.waitForExistence(timeout: 12), "new note should appear in All Notes")

        // Open it, then type into the Markdown editor.
        untitled.tap()
        let editor = app.textViews.firstMatch
        XCTAssertTrue(editor.waitForExistence(timeout: 12), "editor should open after tapping a note")
        // Primary icon-only editor controls expose stable identifiers (a11y).
        XCTAssertTrue(app.buttons["toggleTodoButton"].waitForExistence(timeout: 5),
                      "todo-toggle toolbar control should be reachable by identifier")
        XCTAssertTrue(app.buttons["pinButton"].exists, "pin toolbar control should be reachable by identifier")
        XCTAssertTrue(app.buttons["trashButton"].exists, "trash toolbar control should be reachable by identifier")
        editor.tap()
        editor.typeText("  - [ ] task created on iOS")

        // Pop back so the editor's version-aware flush persists the write. On
        // compact (iPhone) layout the editor is pushed from the All Notes list,
        // so the generated back button carries that list's title as its
        // semantic label — matched by label, not position (`element(boundBy:)`).
        let back = app.navigationBars.buttons["All Notes"]
        XCTAssertTrue(back.waitForExistence(timeout: 8))
        back.tap()

        // Terminate and relaunch against the same store dir.
        app.terminate()
        let relaunched = XCUIApplication()
        relaunched.launchEnvironment["KIEM_DATA_DIR"] = app.launchEnvironment["KIEM_DATA_DIR"]!
        relaunched.launch()
        XCTAssertTrue(relaunched.staticTexts["Untitled"].waitForExistence(timeout: 20),
                      "note should survive a full relaunch")

        // Reopen it and confirm the Markdown edit persisted through the store.
        relaunched.staticTexts["Untitled"].firstMatch.tap()
        let reopened = relaunched.textViews.firstMatch
        XCTAssertTrue(reopened.waitForExistence(timeout: 12))
        let text = (reopened.value as? String) ?? ""
        XCTAssertTrue(text.contains("task created on iOS"),
                      "edited Markdown body should survive relaunch (got: \(text))")
    }

    /// The app opens a Rust store in the sandbox and shows the empty state.
    @MainActor
    func testLaunchesToEmptyStateOnScratchStore() throws {
        let (app, _) = launchApp()
        app.launch()
        XCTAssertTrue(app.staticTexts["No notes yet"].waitForExistence(timeout: 20),
                      "fresh store should show the All Notes empty state")
    }

    /// The shell renders its primary controls and a created note lists under
    /// All Notes on the compact (iPhone) layout.
    @MainActor
    func testCompactShellComposeAndList() throws {
        let (app, _) = launchApp()
        app.launch()
        XCTAssertTrue(app.buttons["composeButton"].waitForExistence(timeout: 20))
        XCTAssertTrue(app.buttons["syncButton"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["No notes yet"].waitForExistence(timeout: 10))

        app.buttons["composeButton"].tap()
        XCTAssertTrue(app.staticTexts["Untitled"].waitForExistence(timeout: 12),
                      "created note should list under All Notes")
    }

    /// The Sync & Pairing sheet is the guided pairing surface: it opens from
    /// the toolbar sync control, shows its guided sections, and — once the
    /// async pairing ticket lands — a copy-code action. Done closes it back to
    /// the list. Deliberately asserts only the stable, user-visible framing;
    /// it does not assert ticket text, QR pixels, or network connectivity.
    @MainActor
    func testSyncPairingSheetShowsGuidedFlow() throws {
        let (app, _) = launchApp()
        app.launch()

        XCTAssertTrue(app.buttons["syncButton"].waitForExistence(timeout: 20),
                      "toolbar sync control should be reachable")
        app.buttons["syncButton"].tap()

        // The sheet is a modal on top of the list; its nav title and guided
        // sections come up quickly once presented.
        let done = app.buttons["Done"]
        XCTAssertTrue(done.waitForExistence(timeout: 10), "Sync & Pairing sheet should open")
        XCTAssertTrue(app.staticTexts["Sync & Pairing"].waitForExistence(timeout: 5),
                      "sheet navigation title should read 'Sync & Pairing'")
        XCTAssertTrue(app.staticTexts["This device"].waitForExistence(timeout: 5),
                      "'This device' guided section should be visible")
        XCTAssertTrue(app.staticTexts["Pair a device"].waitForExistence(timeout: 5),
                      "'Pair a device' guided section should be visible")

        // The pairing controls live far down a lazy, scrollable Form whose lower
        // cells are only materialized when scrolled into view, and the ticket
        // that unlocks copy-code is generated asynchronously. Reveal the "Pair a
        // device" section with at most one controlled swipe — the code cell
        // (copy-code) sits at the top of that section, so a single swipe brings
        // it into view and keeps it materialized while we wait for the async
        // ticket. Crucially we stop there and never overscroll to the bottom of
        // the Form: repeat-swiping scrolls the section above the fold and
        // de-materializes the not-yet-present copy-code control before the
        // ticket can land — the regression this guards against.
        let copyCode = app.buttons["copy-code"]
        if !copyCode.exists {
            // A full-viewport swipeUp overscrolls this short landscape sheet and
            // scrolls the copy-code row past the top edge, de-materializing it
            // (the regression this guard guards against). Use a bounded drag
            // instead: scroll just enough to lift the "Pair a device" section's
            // copy-code row into view without flinging past it.
            let form = app.collectionViews.firstMatch
            let start = form.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.85))
            let end = form.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.4))
            start.press(forDuration: 0.05, thenDragTo: end)
        }
        XCTAssertTrue(copyCode.waitForExistence(timeout: 30),
                      "copy-code action should appear once the async pairing ticket is ready")

        done.tap()
        XCTAssertTrue(done.waitForNonExistence(timeout: 5),
                      "tapping Done should close the Sync & Pairing sheet")
    }
}
