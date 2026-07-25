# Changelog

## Unreleased

- Fixed: the app could freeze for seconds during ordinary note actions — adding a tag, deleting, pinning, creating a note — on a store with hundreds of notes and sync running. Every call into the core ran on the main thread and waited on the same lock the background sync round holds for its whole tick, so anything landing mid-tick blocked the whole UI. Those calls now run off the main thread, and the window stays responsive while they complete. The underlying per-tick cost is unchanged, so a slow action can still take a moment — it just no longer freezes the app while it does.
- Added: `KIEM_SYNC_TRACE=1` makes `kiem sync` print per-round sync diagnostics to stderr (documents offered, frames sent and received, time spent waiting on the store lock versus working). Off by default, and intended for diagnosing a stalled sync against a large store.

## 0.2.2 - 2026-07-22

- Fixed: `kiem` could refuse to start at all — `NoteStore::open_dir` failing with "Operation not supported on socket" — the first time a new version opened a data dir where a sync daemon's `control.sock` was still on disk (e.g. after the daemon was killed rather than exiting cleanly). The one-time backup taken on a version upgrade copied every file in the data dir including that socket, and `fs::copy` can't copy a socket. Non-regular files (sockets, FIFOs) are now skipped in that backup instead of failing it.

## 0.2.1 - 2026-07-22

- Fixed: creating, editing, deleting, or restoring a note could fail with a "Something went wrong" search-index error even though the change had already saved successfully — most likely with a `kiem` CLI command and the app running against the same data dir at once. The search index is a derived, rebuildable structure; a transient index-update failure is now logged and skipped instead of failing the whole operation.

## 0.2.0 - 2026-07-22

- Added: `scripts/sync-agent/` packages `kiem sync` as a per-user launchd LaunchAgent, for running sync unattended on a headless Mac (no GUI app open, no one logged in interactively — just auto-login). `install.sh` sets it up and guards against installing alongside a running GUI app; `uninstall.sh` reverses it. See `scripts/sync-agent/README.md`.
- Fixed: the CLI and the app could both bind a P2P mesh to the same device identity at once (e.g. the GUI app open while `kiem sync` or `kiem pair` also runs) — two accept/dial loops on one identity corrupted peer discovery. Starting a mesh now takes an exclusive lock for the data dir, so a second attempt fails clearly instead of silently colliding.
- Fixed: syncing after both peers restart around the same time could take minutes instead of seconds on a store with hundreds of notes. Every note received during a sync burst was committing the search index individually (a real disk flush + reader reload each time); it now batches those commits once per sync tick instead of once per note.
- Fixed: a single hiccup during sync (e.g. a transient local error unrelated to the connection) could permanently stop syncing with that peer until the app restarted. Sync now logs and continues past a non-connection error instead of tearing down the session.
- Fixed: a fully caught-up peer in the Sync settings pane could get stuck showing "Syncing" forever. The periodic sync ticker was reporting activity on every round even when it had nothing to send; it now only reports activity when data actually goes out, and the pane re-checks the status every second so it settles back to "Connected" a couple seconds after the last real exchange.
- Fixed: long peer ids in the Sync settings no longer overflow the fixed-width window. The pane now scrolls vertically, the settings window can be resized wider, and ids are truncated in the middle with the full value available on hover.

## 0.1.0 - 2026-07-19

- Added: `kiem://note/<id>` references round-trip between the app and the terminal. Right-click a note (or multi-selection) in the app and choose "Copy Reference" to copy `kiem://note/<id>` to the pasteboard; paste it in a terminal and cmd+click to open that note in Kiem. The `kiem` CLI accepts these references anywhere it takes a note id (`show`, `edit`, `delete`, `todo`, `note set-type`, `bulk --id`, etc.). Trashed-note references open the note in the Trash filter, and the app stashes an incoming reference if the store is still starting up.
- Added: the Sync settings pane now shows a per-peer list with human-readable device names, connection status, and a transient "Syncing" indicator when traffic is flowing. You can also rename this device; the new name is sent to peers during the next handshake.
- Fixed: a pending editor edit could clobber a note body that just arrived via P2P sync or an external CLI write. The app now reloads the editor from the store when the open note changed externally and the user isn't actively typing.
- Fixed: a pending editor edit could freeze the app when an import/export started — the debounced save is now flushed before the transfer takes the store lock. The transfer's summary alert also waits for the progress sheet to fully close, so it can no longer be dropped.
- Fixed: notes in the Trash can no longer be dragged onto projects, tags, or Pinned — trashed notes only restore, matching the right-click menu. Sidebar rows that take no drops (Todo, Today, Untagged) no longer show an accepting drag cursor.
- Fixed: a panic or commit failure during a bulk import no longer leaves the store with an open transaction and search disabled — the transaction rolls back and the search index is restored.
