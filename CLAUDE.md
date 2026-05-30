# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repo is

`kiem-app` is the **Kiem product**: a peer-to-peer notes app built on a portable Rust
core with a (future) SwiftUI UI and CLI. The Rust core owns storage, sync (Automerge
CRDTs over local-network peers), search, and identity, and is the **authority** for
everything persisted or synced. The editor is a separate package, **Pulp**, consumed as
a sibling repo at `../pulp` (see the container `CLAUDE.md` one level up).

This repo also holds all cross-cutting `docs/` for the whole project.

**Current state:** early. Only `kiem-core` exists so far (the Tier-1 content-derivation
module). `kiem-cli`, `kiem-ffi`, and the `apple/` SwiftUI app are later phases described
in `docs/plans/`.

## Commands

Run from this repo root (`kiem-app/`):

```bash
cargo test                       # all tests
cargo test -p kiem-core          # one crate
cargo test --test content_fixtures   # one integration-test file
cargo test derivation_matches    # one test by name substring
cargo build
cargo clippy --all-targets       # lint (keep clean)
```

The user's global setup pipes Swift builds through `xcsift`; that does not apply to this
Rust repo. Plain `cargo` is correct here.

## Architecture

**Cargo workspace** (`Cargo.toml`) with member crates under `crates/`. Today that is
just `kiem-core`; the plan adds `kiem-cli` (binary) and `kiem-ffi` (UniFFI cdylib).

**`kiem-core`** is a pure Rust library — zero FFI awareness, testable with `cargo test`
alone. Its current scope is the `content` module (`crates/kiem-core/src/content.rs`):
`derive_title`, `extract_tags`, `has_unchecked_todos` — deriving note metadata from
Markdown body text.

### The cross-language parity contract (the thing to understand before touching `content`)

`kiem-core::content` is the **authoritative** implementation of title/tag/todo
derivation. Pulp's Swift `ContentAnalyzer` is a second implementation of the same rules
that must produce **byte-identical** results. They are bound by a shared, language-neutral
fixture file:

- `fixtures/content-derivation.json` — the canonical contract (`{input, title, tags, hasUncheckedTodos}` cases).
- `crates/kiem-core/tests/content_fixtures.rs` runs every case against the Rust impl.
- Pulp's `Tests/PulpTests/ContentFixtureTests.swift` runs the same cases against a
  vendored copy in the Pulp repo.

**To change any derivation rule:** edit the Rust impl, edit the Swift impl in `../pulp`,
edit `fixtures/content-derivation.json`, re-vendor the copy into
`../pulp/Tests/PulpTests/Fixtures/`, and run both test suites. There is deliberately
**no cross-repo filesystem test** here — each repo tests its own copy; keeping them in
sync is a release/CI concern.

### Rust ↔ Swift porting gotchas (already cost real bugs)

The two implementations use different regex engines and string models. When changing
derivation logic, preserve these:

- Rust's `regex` crate has **no lookbehind and no backreferences** (Swift's
  `NSRegularExpression` has both). Swift's `(?<=\s|^)` lookbehind is reimplemented as an
  explicit preceding-char check; a `\1` backreference became a manual line scan.
- **Unicode whitespace:** mirror Swift's `\s`/`.whitespaces` (which match NBSP, ideographic
  space, etc.) with Unicode-aware predicates — an ASCII-only check silently drops tags.
- Normalize `\r\n` → `\n` before processing (both sides do).

## Conventions

- File-size limit: 500 lines — split into modules at the limit.
- `docs/plans/` is **gitignored** (treated as local build artifacts). Don't expect plan
  files to be tracked here; they still exist on disk and are the roadmap.
- Dated doc filenames keep their historical `pear-` prefix; new files use `kiem`.
