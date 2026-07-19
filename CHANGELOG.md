# Changelog

## Unreleased

- Fixed: a pending editor edit could freeze the app when an import/export started — the debounced save is now flushed before the transfer takes the store lock. The transfer's summary alert also waits for the progress sheet to fully close, so it can no longer be dropped.
- Fixed: notes in the Trash can no longer be dragged onto projects, tags, or Pinned — trashed notes only restore, matching the right-click menu. Sidebar rows that take no drops (Todo, Today, Untagged) no longer show an accepting drag cursor.
- Fixed: a panic or commit failure during a bulk import no longer leaves the store with an open transaction and search disabled — the transaction rolls back and the search index is restored.

## 0.1.0-alpha.17 - 2026-07-18

- Fixed: copying a pairing code from one Kiem app and adding it in another no longer fails with a misleading “Check the code” error. The app now schedules pairing work on its existing Tokio runtime.
- Improved: the pairing-code input is left-aligned, monospaced, and accepts visually wrapped pasted codes.
- Improved: after moving an open note to Trash, the next remaining note stays selected.

## 0.1.0-alpha.16 - 2026-07-18

- Added: multi-select in the note list — ⌘-click/⇧-click select several notes, and every action works the same through right-click or drag-to-sidebar: drag to Trash or right-click > Move to Trash; drag onto a project or right-click > Add to Project; drag onto a tag to tag; drag onto Pinned or right-click > Pin. Restore in Trash, ⌫/⌘⌫, and the editor pane's "N notes selected" placeholder all follow the selection.
- Added: `kiem bulk` safely applies tag, type, trash, and restore operations to notes selected by tag, project, repeated IDs, or stdin. Bulk changes support dry runs, require explicit confirmation, verify note versions, and commit atomically.
- Fixed: importing a big folder no longer freezes the app — a 400-note import dropped from minutes to under a second (the duplicate check re-read every existing note per file, and every note paid its own database and search-index commit; imports now run as one transaction with one index rebuild). The app runs transfers in the background and shows a progress bar while they run.
- Changed: importing no longer forces notes into projects. The app now asks: "Folders Are Projects" keeps the old behavior, "Just Import Notes" imports a plain dump (e.g. from Bear) with only the tags the notes already carry. The CLI equivalent is `kiem import --no-project`.

## 0.1.0-alpha.15 - 2026-07-18

- Added: import/export in the macOS app — File > Import Notes from Folder… / Export All Notes… run the same folder-per-project Markdown exchange as the CLI (the logic moved into the shared core, so both surfaces are one implementation). A summary alert reports how many notes were transferred and skipped.
- Added: `kiem export <dir>` / `kiem import <dir>` — exchange notes (and their todos) as a directory of Markdown files, same layout both directions: a folder is a project. Export writes one subfolder per project with one file per note (body verbatim); `--project` exports one project flat into the directory itself. Import maps subfolders to projects, treats a flat folder as one project named after it (`--project` overrides), and skips exact duplicates so re-importing is a no-op. Import is all-or-nothing: every folder is validated before any note is created, so a folder that can't name a project fails the import cleanly instead of leaving it half-done. A non-default note type (plan, review, …) travels as a `type:` line in the file's frontmatter fence.
- Added: the project breadcrumb above the editor is now clickable — clicking the project name opens that project's note list, same as selecting it in the sidebar.
- Changed: in the sidebar's open-todos list, tapping a todo no longer marks it done — only the circle checkbox completes it. Clicking the todo text now edits it in place (Return or click-away saves, Escape cancels).
- Added: `set_todo_text` in the core/FFI — rename a todo by its (note, index) address through the same sync-safe body-update path as toggling. Line terminators in the new text are collapsed to spaces so a rename can never splice extra lines into the note or shift other todos' indices.

## 0.1.0-alpha.14 - 2026-07-12

- Added: device pairing UI — Settings (⌘,) now has a Sync pane. "Show this Mac" arms a single-use 2-minute pairing window and shows a QR code plus a copyable code; "Add a device" takes a pasted code. One action pairs both devices: trust is exchanged over the connection itself, so nobody pastes two codes.
- Added: an accept-side trust gate — an unknown device dialing in is refused outright unless a pairing window is open *and* you allow it in the "Pair this device?" prompt. Previously any device that knew your endpoint id could connect and sync.
- Added: `kiem pair show` is now arm-and-wait — it prints this device's code, waits for one device to pair, asks for approval at the prompt (`--yes` auto-approves), and exits once paired. `kiem pair add` connects immediately instead of waiting for the next daemon start. With the sync daemon running, both commands drive it through a new control socket (`~/.kiem/control.sock`), which also makes a second `kiem sync` refuse to start instead of racing the first.
- Added: `kiem note add` reads the note body from `--file <path>` or stdin — safe for markdown with backticks or `$(…)` that a shell mangles inside a quoted argument.
- Changed: the sync-status indicator moved from the toolbar into the Settings Sync pane — pairing and status are rare, deliberate actions that don't need main-window chrome.
- Breaking: the sync protocol version is bumped (`kiem-sync/1`) for the pairing handshake — devices only sync with other alpha.14+ installs; update all paired devices together.

## 0.1.0-alpha.13 - 2026-07-11

- Fixed: ⌘⌫ (instant-trash the selected note) never actually fired — the Command-modified Delete key doesn't reach the list's delete handler. It's now caught by a window-local key monitor that yields to text editors. Plain ⌫ (confirm) was unaffected.
- Added: a UI-test target (KiemUITests) covering the note-opens-and-survives-relaunch regression, keyboard deletes, and Empty Trash — the flows synthetic-event scripting can't drive.
- Fixed: sync could silently dead-end right after pairing — QUIC streams open lazily, so when the dialing side had nothing to send (a fresh, empty store) neither side ever spoke and both sat waiting forever. The dialer now always opens with a first sync round, which also cuts first-sync latency by one interval. This was the real cause of the intermittent "paired but nothing syncs" hangs previously blamed on discovery latency.

## 0.1.0-alpha.12 - 2026-07-10

- Added: Delete Project — right-click a project in the sidebar to permanently erase it and every note tagged into it, trashed ones included. Always asks for confirmation (it can't be undone), and uses the same sync-safe tombstoning as Empty Trash.
- Added: Empty Trash — a button at the bottom of the Trash listing and a right-click menu on the sidebar's Trash item both permanently erase everything in the trash (after one confirmation; this can't be undone). Purged notes are tombstoned in the store so a later sync exchange with a device that still holds them can't resurrect them.
- Added: delete from the note list with the keyboard — ⌘⌫ moves the selected note to Trash instantly, plain ⌫ asks for confirmation first. Both only apply while the list has focus; in the editor the keys keep their text-editing meanings.
- Added: open todos are grouped by source note — subtle captions divide the runs so each todo shows which plan/doc it came from. Applies to a project's "Open todos" panel and to the Todo smart filter, which now lists the actual todos (tap to complete) instead of just the notes containing them.
- Added: permanent deletes sync — Empty Trash / Delete Project record their erasures in a tombstone document that rides the normal peer sync, so a purge on one device erases the same notes on every paired device (an offline edit to a purged note loses to the purge). Requires this version on both devices.
- Added: sync-status indicator in the toolbar (U13) — connected/paired device count with a tooltip naming connected peers; hidden until a device is paired.
- Changed: pairing tickets now wait briefly for relay registration so tickets carry a live relay hint, cutting cold-discovery first-connect latency.

## 0.1.0-alpha.11 - 2026-07-10

- Fixed: the recurring "tap a note, the editor shows nothing" bug for real this time. Root cause was in the Pulp editor, not Kiem: the editor's text view could come up zero points wide (created at `.zero` with no width tracking), so the note laid out in an invisible column while the text was present all along. The alpha.4/alpha.7 window-restoration fixes and the alpha.9 refresh guard were different bugs with the same symptom; this one is fixed at the source in Pulp, with regression tests.
- Changed: Pulp's editor is now TextKit 2-native (`NSTextLayoutManager`); the dead TextKit 1 layout-manager subclass is deleted.
- Changed: modern-toolchain baseline — macOS 26 deployment target, Swift 6 language mode (app and Pulp), and the release workflow now builds on `macos-26` so shipped binaries link the same SDK generation dev builds are tested against.
- Changed: releases pin the Pulp checkout to the commit in `pulp.ref` instead of floating on Pulp's main branch, so a release is reproducible.
- Removed: the leftover `NSBonjourServices` `_kiem._tcp` declaration (the Bonjour discovery stack left in alpha.10's iroh migration).

## 0.1.0-alpha.10 - 2026-07-06

- Added: app sync now uses the shared `kiem-sync` iroh mesh directly, so the macOS app and CLI share the same pairing/relay transport instead of the old Bonjour stack.
- Added: newly-created notes are authored by the device's iroh EndpointId instead of the placeholder `local`, giving synced notes stable per-device authorship.
- Improved: editor persistence is debounced (one store write per pause, not per keystroke) and sidebar smart-filter counts are computed in one scan.
- Maintenance: split oversized core/CLI/FFI files under the 500-line limit and removed the unused framed-TCP codec.

## 0.1.0-alpha.9 - 2026-07-05

- Fixed: the editor pane could go blank (no data loss) when a store refresh raced a sync or external write — a transient DB read failure emptied the note list and spuriously cleared the selection. A failed refresh now leaves the list and selection untouched.
- Added: the `kiem` CLI now auto-maintains a symlink to the bundled binary on app launch (the VS Code `code` model), so it tracks the installed app version with no user interaction after the one-time install. A shadowing `cargo install`-ed `~/.cargo/bin/kiem` is detected and flagged for removal.
- Polish: tightened the sidebar/editor chrome — static app name in the window title bar (was duplicating the note's H1), reserved `proj/*` tags hidden from note-list rows, rebalanced column widths, a lighter formatting toolbar, 2-line note titles, and a quieter project breadcrumb above the editor.

## 0.1.0-alpha.8 - 2026-07-04

- Fixed: the previous release's window-restoration fix had a gap — File > New Window (⌘N) could still open a second, unprotected window that macOS would restore on relaunch, reintroducing the same blank-editor bug through a different path. Kiem is a single-window app; that menu command is now removed.

## 0.1.0-alpha.7 - 2026-07-04

- Fixed: after quitting and relaunching, a previously-open note could appear selected (title bar, highlighted row) while the editor showed nothing. This is a recurrence of the 0.1.0-alpha.4 fix — that fix didn't actually work, it changed the wrong setting. macOS was still restoring window/selection state on relaunch regardless; the editor just never got told to load the restored note. Window state restoration is now actually disabled.

## 0.1.0-alpha.6 - 2026-07-04

- Fixed: opening any note created before 0.1.0-alpha.5 crashed with a "Something went wrong / Storage(... document error ... unexpected nothing at all, expected a ScalarValue::Null)" alert. 0.1.0-alpha.5 added a new `status` field to every note's metadata; every note from before that release has no `status` key in its document at all, and Automerge hydration treated that as an error instead of "not set." Fixed at the source — hydrating a note that predates the field now correctly reads as no status, matching every other optional field added over time.

## 0.1.0-alpha.5 - 2026-07-04

- Fixed: Kiem.app's sidebar and an already-open project view could go stale when the shared store changed on disk outside the app's own actions (e.g. an external `kiem` CLI write) — the list wouldn't update until you navigated away and back. The app now watches the store for external writes and refreshes automatically.
- Added: a note's project now shows in a status bar above the editor instead of only as an inline `#proj/<slug>` tag in the body. A note can also carry a `status: active` / `status: completed` frontmatter line (plain markdown, not Kiem-specific); when present, it shows in that same status bar and, in a project's note list, replaces the (redundant) project tag with a status badge. Pulp renders the frontmatter fence itself as a callout instead of plain text between two horizontal rules.

## 0.1.0-alpha.4 - 2026-07-03

- Fixed: after quitting and relaunching, the editor pane could show nothing for a note that visually looked selected — no data was ever lost, but the note's content wouldn't display until you clicked another note and back. Caused by macOS's automatic window/selection restoration desyncing from the app's own state; the app now manages its own window state exclusively.

## 0.1.0-alpha.3 - 2026-07-03

- Release checksum files (`.sha256`) now use a portable relative filename instead of the CI runner's absolute build path, so `shasum -c` actually works after downloading a release.
- Real changelog process going forward: release notes are now generated from this file instead of a generic placeholder (see `docs/RELEASING.md`).

## 0.1.0-alpha.2 - 2026-07-03

- Notarized, Developer ID signed release — installs with no Gatekeeper warnings, replacing the previous ad-hoc-signed build that needed a right-click-Open workaround.
- Editing a note down to zero tags is now rejected instead of silently dropping it out of every tag/project filter (`kiem edit`/`kiem edit-lines`).
- Data directory gets a version-stamped safety backup: if a future release changes the on-disk format, the whole `~/.kiem` directory is copied to a timestamped sibling before anything touches it.
- Fixed a real P2P sync deadlock: two paired devices dialing each other simultaneously could establish two independent connections where each side committed to a different one, so sync silently never completed. Only the lower device id dials now; the other side just accepts.
- `kiem-ffi` gained a UniFFI surface for starting/stopping sync, pairing devices, and reading connected peers, for the macOS app to use directly instead of driving the byte-level sync protocol itself.

## 0.1.0-alpha.1 - 2026-07-03

- First downloadable build of the macOS app, ad-hoc signed (needs a right-click-Open to bypass Gatekeeper — fixed in 0.1.0-alpha.2).
- P2P sync transport rewritten on [iroh](https://docs.iroh.computer/) (QUIC), replacing local-network-only mDNS — devices now sync across different networks, not just the same LAN.
- New `kiem pair show` / `kiem pair add <ticket>` commands to explicitly trust a device for sync, replacing automatic LAN discovery.
