# Changelog

## 0.1.0 - 2026-07-19

- Added: `kiem://note/<id>` references round-trip between the app and the terminal. Right-click a note (or multi-selection) in the app and choose "Copy Reference" to copy `kiem://note/<id>` to the pasteboard; paste it in a terminal and cmd+click to open that note in Kiem. The `kiem` CLI accepts these references anywhere it takes a note id (`show`, `edit`, `delete`, `todo`, `note set-type`, `bulk --id`, etc.). Trashed-note references open the note in the Trash filter, and the app stashes an incoming reference if the store is still starting up.
- Added: the Sync settings pane now shows a per-peer list with human-readable device names, connection status, and a transient "Syncing" indicator when traffic is flowing. You can also rename this device; the new name is sent to peers during the next handshake.
- Fixed: a pending editor edit could clobber a note body that just arrived via P2P sync or an external CLI write. The app now reloads the editor from the store when the open note changed externally and the user isn't actively typing.
- Fixed: a pending editor edit could freeze the app when an import/export started — the debounced save is now flushed before the transfer takes the store lock. The transfer's summary alert also waits for the progress sheet to fully close, so it can no longer be dropped.
- Fixed: notes in the Trash can no longer be dragged onto projects, tags, or Pinned — trashed notes only restore, matching the right-click menu. Sidebar rows that take no drops (Todo, Today, Untagged) no longer show an accepting drag cursor.
- Fixed: a panic or commit failure during a bulk import no longer leaves the store with an open transaction and search disabled — the transaction rolls back and the search index is restored.
