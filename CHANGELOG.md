# Changelog

## Unreleased

- Added: open todos are grouped by source note — subtle captions divide the runs so each todo shows which plan/doc it came from. Applies to a project's "Open todos" panel and to the Todo smart filter, which now lists the actual todos (tap to complete) instead of just the notes containing them.

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
