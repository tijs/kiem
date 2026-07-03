# Changelog

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
