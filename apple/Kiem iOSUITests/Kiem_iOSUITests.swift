import UIKit
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

    /// The Sync button now opens the general Sync Status screen (not the pairing
    /// form). This taps it, waits for the status screen, then navigates the
    /// "Set Up New Device" action into the pairing page where the pairing
    /// controls live. Shared by every pairing test so pairing is reached the
    /// same way a real user does — through the one-time setup navigation.
    @MainActor
    private func openSetUpNewDevice(in app: XCUIApplication) {
        let sync = app.buttons["syncButton"]
        XCTAssertTrue(sync.waitForExistence(timeout: 20), "toolbar sync control should be reachable")
        sync.tap()
        XCTAssertTrue(app.navigationBars["Sync Status"].waitForExistence(timeout: 10),
                      "Sync button should open the Sync Status sheet")
        let pairingLink = app.descendants(matching: .any)["startPairing"].firstMatch
        XCTAssertTrue(pairingLink.waitForExistence(timeout: 5),
                      "'Set Up New Device' action should be reachable on the status screen")
        pairingLink.tap()
        XCTAssertTrue(app.staticTexts["This device"].waitForExistence(timeout: 10),
                      "Set Up New Device page should show the pairing form")
    }

    /// Reveals the async pairing ticket's copy-code control. The pairing Form's
    /// lower cells are lazy and only materialized when scrolled into view, and
    /// the ticket that unlocks copy-code arrives asynchronously. A bounded drag
    /// (never a full swipe) lifts the "Pair a device" section's top row into
    /// view without flinging past it — repeat-swiping would de-materialize the
    /// not-yet-ready copy-code control (the overscroll trap).
    @MainActor
    private func revealCopyCode(in app: XCUIApplication) {
        let copyCode = app.buttons["copy-code"]
        if !copyCode.exists {
            let form = app.collectionViews.firstMatch
            let start = form.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.85))
            let end = form.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.4))
            start.press(forDuration: 0.05, thenDragTo: end)
        }
        XCTAssertTrue(copyCode.waitForExistence(timeout: 30),
                      "copy-code action should appear once the async pairing ticket is ready")
    }

    /// The Sync button opens a general sync/connection status screen, not the
    /// pairing form: a status summary (mesh running / paired / connected) is
    /// visible, the screen is dismissible, and merely opening it must NOT
    /// auto-present the pairing form (pairing is a one-time action gated behind
    /// the "Set Up New Device" navigation).
    @MainActor
    func testSyncButtonOpensStatusScreen() throws {
        let (app, _) = launchApp()
        app.launch()

        XCTAssertTrue(app.buttons["syncButton"].waitForExistence(timeout: 20))
        app.buttons["syncButton"].tap()

        // Status sheet opens with its title and an honest state summary.
        XCTAssertTrue(app.navigationBars["Sync Status"].waitForExistence(timeout: 10),
                      "Sync button should open the Sync Status sheet")
        XCTAssertTrue(app.descendants(matching: .any)["sync-overall-state"].waitForExistence(timeout: 5),
                      "an overall sync-state summary should be visible on the status screen")
        XCTAssertTrue(app.descendants(matching: .any)["sync-known-count"].waitForExistence(timeout: 5),
                      "the paired-device count should be visible")

        // Opening the status screen is NOT opening the pairing form: no auto-arm.
        XCTAssertFalse(app.staticTexts["This device"].exists,
                       "opening the status screen must not show the pairing form")

        // Dismissible from the sheet via Done.
        app.buttons["Done"].tap()
        XCTAssertTrue(app.buttons["syncButton"].waitForExistence(timeout: 5),
                      "Done should dismiss the Sync Status sheet back to the list")
    }

    /// The subordinate "Set Up New Device" action reaches the pairing controls,
    /// and the nested pairing page navigates back cleanly to the status screen.
    @MainActor
    func testSetUpNewDeviceReachesPairingControls() throws {
        let (app, _) = launchApp()
        app.launch()

        openSetUpNewDevice(in: app)

        // The pairing form's guided sections are present behind the navigation.
        XCTAssertTrue(app.staticTexts["This device"].exists,
                      "Set Up New Device page should show the pairing form")

        // Navigate back cleanly to the Sync Status screen (not straight out of
        // the sheet).
        let back = app.navigationBars.buttons.element(boundBy: 0)
        XCTAssertTrue(back.exists, "nested pairing page should offer a back button")
        back.tap()
        XCTAssertTrue(app.navigationBars["Sync Status"].waitForExistence(timeout: 5),
                      "back should return to the Sync Status sheet")
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
        let editor = app.textViews["note-editor"]
        XCTAssertTrue(editor.waitForExistence(timeout: 12), "editor should open after tapping a note")
        // Primary icon-only editor controls expose stable identifiers (a11y).
        XCTAssertTrue(app.buttons["toggleTodoButton"].waitForExistence(timeout: 5),
                      "todo-toggle toolbar control should be reachable by identifier")
        XCTAssertTrue(app.buttons["pinButton"].exists, "pin toolbar control should be reachable by identifier")
        XCTAssertTrue(app.buttons["trashButton"].exists, "trash toolbar control should be reachable by identifier")
        editor.tap()
        editor.typeText("  - [ ] task created on iOS")

        // The keyboard must be dismissible without relying on an invisible
        // swipe; the accessory Done action should reveal the navigation controls
        // again and leave the editor usable.
        let editorDone = app.buttons["editorDone"]
        XCTAssertTrue(editorDone.waitForExistence(timeout: 8),
                      "editor keyboard toolbar should expose a Done action")
        editorDone.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 8),
                      "editor Done should dismiss the keyboard")

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
        let reopened = relaunched.textViews["note-editor"]
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

    /// The sync sheet's guided pairing flow: the Sync button opens the Sync
    /// Status screen; the "Set Up New Device" action leads to the pairing page;
    /// once the async pairing ticket lands a copy-code action is available; and
    /// navigating back then Done closes the sheet. Deliberately asserts only the
    /// stable, user-visible framing — never ticket text, QR pixels, or network
    /// connectivity.
    @MainActor
    func testSyncPairingSheetShowsGuidedFlow() throws {
        let (app, _) = launchApp()
        app.launch()

        // Status screen opens from the toolbar Sync button.
        XCTAssertTrue(app.buttons["syncButton"].waitForExistence(timeout: 20),
                      "toolbar sync control should be reachable")
        app.buttons["syncButton"].tap()
        XCTAssertTrue(app.navigationBars["Sync Status"].waitForExistence(timeout: 10),
                      "Sync button should open the Sync Status sheet")

        // Navigate the guided flow: status → Set Up New Device → pairing form.
        XCTAssertTrue(app.staticTexts["Set Up New Device"].waitForExistence(timeout: 5),
                      "'Set Up New Device' action should be visible on the status screen")
        app.descendants(matching: .any)["startPairing"].firstMatch.tap()
        XCTAssertTrue(app.staticTexts["This device"].waitForExistence(timeout: 10),
                      "'This device' guided section should be visible on the pairing page")
        XCTAssertTrue(app.staticTexts["Pair a device"].waitForExistence(timeout: 5),
                      "'Pair a device' guided section should be visible")

        revealCopyCode(in: app)

        // Navigate back to the status screen, then dismiss the sheet via Done.
        let back = app.navigationBars.buttons.element(boundBy: 0)
        XCTAssertTrue(back.exists, "nested pairing page should offer a back button")
        back.tap()
        XCTAssertTrue(app.navigationBars["Sync Status"].waitForExistence(timeout: 5),
                      "back should return to the Sync Status sheet")
        app.buttons["Done"].tap()
        XCTAssertTrue(app.buttons["syncButton"].waitForExistence(timeout: 5),
                      "tapping Done should close the sheet back to the list")
    }

    /// The Copy action copies directly to the pasteboard and NEVER presents the
    /// share/activity sheet (Share remains a separate ShareLink action). Asserts
    /// the positive copy confirmation and that no activity sheet took over the
    /// app — without ever capturing/logging the secret ticket text. Pairing is
    /// reached the same way as a real user: through the status screen's
    /// "Set Up New Device" action.
    @MainActor
    func testCopyPairingCodeCopiesToPasteboardWithoutShareSheet() throws {
        let (app, _) = launchApp()
        app.launch()

        openSetUpNewDevice(in: app)
        revealCopyCode(in: app)

        let copyCode = app.buttons["copy-code"]
        copyCode.tap()

        // Positive copy confirmation. This is the decisive regression signal:
        // the Copy button's action writes the ticket to the pasteboard and sets
        // the transient confirmation. If Copy were wrongly wired to launch the
        // share/activity sheet instead, this confirmation would never appear and
        // the app would be covered by the system sheet — so its appearance proves
        // both that the direct copy ran and that no share sheet took over.
        XCTAssertTrue(app.staticTexts["Code copied"].waitForExistence(timeout: 3),
                      "tapping copy should show the transient copy confirmation, not a share sheet")

        // The pairing ticket lands on the pasteboard. Read it without asserting
        // on the unrelated sheet-dismissal lifecycle.
        XCTAssertTrue(UIPasteboard.general.hasStrings,
                      "copying the code must place a string on the pasteboard")
    }

    @MainActor
    func testEditorKeyboardCanBeDismissedWithoutLosingControls() throws {
        let (app, _) = launchApp()
        app.launch()

        XCTAssertTrue(app.staticTexts["No notes yet"].waitForExistence(timeout: 20))
        app.buttons["composeButton"].tap()
        let untitled = app.staticTexts["Untitled"]
        XCTAssertTrue(untitled.waitForExistence(timeout: 12))
        untitled.tap()

        let editor = app.textViews["note-editor"]
        XCTAssertTrue(editor.waitForExistence(timeout: 12))
        editor.tap()
        editor.typeText("  - [ ] keyboard dismissal")

        let done = app.buttons["editorDone"]
        XCTAssertTrue(done.waitForExistence(timeout: 8),
                      "editor keyboard toolbar should expose a Done action")
        done.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 8),
                      "editor Done should dismiss the keyboard")
        XCTAssertTrue(app.buttons["toggleTodoButton"].exists,
                      "editor controls should remain reachable after dismissal")
        XCTAssertTrue(app.buttons["pinButton"].exists,
                      "pin control should remain reachable after dismissal")
    }
}
