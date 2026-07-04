# Changelog

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
