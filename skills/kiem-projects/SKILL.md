---
name: kiem-projects
description: >-
  Work on a software project whose notes, todos, and progress live in Kiem (a P2P
  notes store) instead of scattered across the repo. Use at the start of any
  session in a repo that contains a `.kiem` marker, or when the user mentions Kiem
  project state, project todos, or asks you to record/track progress in Kiem. The
  interface is the `kiem` CLI; project state is shared across agents, machines, and
  the Kiem app.
---

# Kiem projects

Kiem is the project's shared memory. Its notes and todos sync across machines and
are readable by any agent and by the human (in the Kiem GUI). When you work in a
Kiem-bound repo, **read project state from `kiem` first** — do not reconstruct it
from code or `CLAUDE.md` — and **record progress back into `kiem`** so the next
agent (or the human on another device) picks up where you left off.

A project is the reserved tag `proj/<slug>`. A repo declares its project in a
committed `.kiem` marker; the CLI resolves the current project from it
automatically (see `docs/specs/kiem-project-marker.md`). You rarely type the tag —
just run `kiem` from inside the repo.

## At the start of a session

1. Confirm the project: `kiem project current` (prints `proj/<slug>`; if it errors,
   the repo isn't onboarded — see "Onboarding" below).
2. Read the state instead of re-deriving it:
   - `kiem todos` — the open task list (each line: `<note-id>  <index>  <text>`).
   - `kiem notes` — the project's notes (decisions, context, plans, learnings).
   - `kiem show <note-id>` — the full body of any note.

   Add `--json` to any read for structured output.

That is your ground truth for "what is this project and what's left."

## While working

- Complete a task: `kiem todo check <note-id> <index>` (undo with `uncheck`).
  Addresses come straight from `kiem todos`. Indices are positional within a note,
  so **re-run `kiem todos` before acting** if the note may have changed.
- Record a decision, finding, or new plan: `kiem note add "<markdown text>"`. The
  note is tagged into the current project automatically. The first line is the
  title; include `- [ ]` lines to add new todos.
- Keep notes small and purposeful — one decision or task list per note — so the
  human can scan them in the app.

## Onboarding a repo (only if `kiem project current` fails or the user asks)

`kiem project add "<Project Name>"` — run from the repo root. It writes the
committed `.kiem` marker, adds an `AGENTS.md` pointer, and creates a home note.
Idempotent: re-running just (re)binds this directory.

## Notes

- The interface is the CLI; everything you write syncs to the user's other devices
  (including their phone) within moments — treat notes as user-visible.
- This skill is the Claude flavor of an agent-agnostic contract: any tool that can
  run `kiem` can participate. The durable spec is `docs/specs/kiem-project-marker.md`.
- North star (not yet built): brainstorm/plan/review/compound docs will live as
  typed project notes here too, making the whole compound-engineering loop portable.
