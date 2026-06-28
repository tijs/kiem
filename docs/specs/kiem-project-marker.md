# Kiem project marker (`.kiem`)

Durable contract for how a repository declares which Kiem project it belongs to,
and how the `kiem` CLI resolves the "current project" for an agent working in that
repo. See also `docs/specs/markdown-format.md` (tag grammar) and the requirements
doc `docs/brainstorms/2026-06-28-projects-and-agent-orchestration-requirements.md`.

## What a project is

A project is the reserved tag **`proj/<slug>`**. A note belongs to a project when
its body carries that tag (plain-markdown membership — nothing else is stored).
A project's task list is the aggregate of unchecked `- [ ]` items across its notes.

Kiem stores **no filesystem paths** and no per-machine registry. The only binding
between a repo on disk and a project is the committed marker below, which travels
with the repo via git — so the same project can live at different paths on
different machines, or be checked out nowhere (iOS), and still resolve.

## The marker file

A repo declares its project in a committed file named **`.kiem`** at its root:

```
project = "proj/<slug>"
```

- Format: one `project = "<tag>"` line. Surrounding whitespace and the quotes are
  tolerated; the first valid `project` key wins. (TOML-compatible on purpose, so a
  richer `.kiem` can grow later without breaking readers.)
- Committed to the repo so it is portable across machines. No absolute paths.
- Written by `kiem project add <name>`; safe to edit or commit by hand.

## Slugs

`slugify(name)` (canonical implementation: `crates/kiem-cli/src/project.rs`):
lowercase **ASCII A–Z only** (non-ASCII characters are dropped, not Unicode
case-folded); keep `[a-z0-9/]`; space / `-` / `_` → a single `_`; drop everything
else; collapse repeats; trim leading/trailing `_`. An empty slug yields an empty
tag (callers reject it rather than create a degenerate `proj/`). The separator is
`_`, **not** `-`, because the tag grammar (`#([a-zA-Z][a-zA-Z0-9_/]*)`) rejects
`-` — a `-` slug would not round-trip through tag derivation. Example:
`"Kiem App"` → `proj/kiem_app`.

The macOS app mirrors this rule in `KiemModel.projectTag(for:)`. The two are kept
in lockstep by the language-neutral `fixtures/project-slug.json` parity contract
(the same pattern as content derivation): the Rust CLI test suite asserts against
it today; the Swift side vendors and asserts against it once an app test target
exists. The ASCII-only fold matters — `String.lowercased()` in Swift would keep
characters (e.g. Turkish `İ`, Kelvin `K`) that Rust drops, splitting one project
across two tags.

## Resolution precedence

The CLI resolves the current project in this order:

1. An explicit `--project <name-or-tag>` flag.
2. The `.kiem` marker in the working directory **or any ancestor up to the repo
   root** (the ancestor walk stops at the first directory containing `.git`, so a
   stray marker above the repo — e.g. `~/.kiem` — never captures an unrelated repo).
3. Fallback: the slugified name of the working directory.

The explicit marker (2) is preferred over directory-name inference (3); inference
exists only so a repo with no marker still resolves to *something* predictable.
A resolved marker value is canonicalized through the slug rule, so a hand-edited
non-canonical tag (e.g. `proj/My App`) resolves to the same tag that `note add`
embeds (`proj/my_app`) instead of desyncing writes from queries.

## AGENTS.md pointer

`kiem project add` also appends a one-line human pointer to the repo's `AGENTS.md`
(creating it if absent), idempotently:

```
<!-- kiem -->
This repo is Kiem project `proj/<slug>`. Run `kiem todos` / `kiem notes` for
project state, and record progress with `kiem note add` / `kiem todo check`.
```

This is discovery sugar for agents that read `AGENTS.md`; the machine-read binding
is always the `.kiem` marker, never this prose.

## Agent-agnostic by design

The portable interface is the **CLI** (and, later, MCP): any agent or tool that
can run `kiem` can read and maintain project state. The agent integrations —
including the Claude Code skill/plugin — live in the separate `kiem-agent` repo
and are one flavor over this contract, not a requirement of it.
