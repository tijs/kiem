# Kiem CLI

`kiem` is the terminal interface to the Kiem store — the same notes the app
displays, synced across devices. Run `kiem --help` (or `kiem <command> --help`)
for the full command list; this guide covers the task-list commands and their
numbering.

## Todos

A todo is a `- [ ]` checkbox line in a note body. `kiem todos` lists the open
todos for the current project:

```console
$ kiem todos
<note-id>  1  First open task
<note-id>  2  Second open task
```

`kiem todos --json` returns the same information as structured output —
`[{"note_id": "<note-id>", "index": 1, "text": "First open task"}]`.

## Checkbox indexes are one-based

The `index` column numbers checkboxes from **1** (1 = first checkbox), and
`kiem todo check` / `kiem todo uncheck` accept the same numbering:

```console
$ kiem todo check <note-id> 3   # checks the third checkbox
$ kiem todo uncheck <note-id> 3
```

Pass several indexes at once to check them together
(`kiem todo check <note-id> 1 3`).

- **0 is rejected**: `kiem todo check <note-id> 0` fails with an error explaining
  that indexes are 1-based — it never silently toggles the first checkbox.
- **Positions are stable**: indexes count *all* checkbox lines — checked and
  unchecked — so checking one item never renumbers the others.
- **Re-read before acting**: indexes are positional within the note, so if the
  note may have changed since you listed it (another device or the app edited
  it), re-run `kiem todos` immediately before `check` / `uncheck` — a stale
  index can toggle the wrong item.

## Internal numbering is not part of the CLI contract

The core store and FFI address checkboxes with zero-based positions; the CLI
converts to them internally. Treat zero-based numbers as an implementation
detail. The public contract is the one-based numbering `kiem todos` prints, and
0 is an error — not "the first checkbox".
