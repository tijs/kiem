# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repo is

`kiem-app` is the **Kiem product**: a peer-to-peer notes app built on a portable Rust
core with a (future) SwiftUI UI and CLI. The Rust core owns storage, sync (Automerge
CRDTs over local-network peers), search, and identity, and is the **authority** for
everything persisted or synced. The editor is a separate package, **Pulp**, consumed as
a sibling repo at `../pulp` (see the container `CLAUDE.md` one level up).

This repo also holds all cross-cutting `docs/` for the whole project.

**Current state:** all three crates exist (`kiem-core`, `kiem-cli`, `kiem-ffi`) plus a
working `apple/` macOS SwiftUI app. The core owns Automerge note storage, SQLite
denormalized metadata, tantivy search, and P2P sync over an iroh mesh
(`kiem-sync`: ticket pairing, LAN + cross-network with relay fallback). The CLI
does CRUD + a sync daemon. The macOS app does CRUD, tags, smart
filters, full-text search, trash/restore, a formatting toolbar (over the Pulp editor),
and background P2P sync. Identity is the device's iroh `EndpointId` (stamped as
`author_did` on new notes; U11's DIDs and U17's cross-network spike were superseded by
the iroh migration). Remaining roadmap units — tracked in the Kiem "Roadmap" note
(proj/kiem_app), not in repo files: live-sync→editor refresh (U10), sync-status UI
(U13), full CLI flags (U14), MCP (U15), skills setup (U16), and the iOS Pulp port.

**Toolchain baseline (since v0.1.0-alpha.11):** macOS 26 deployment target, Swift 6
language mode, Pulp is TextKit 2-native.

**Releases are cut locally**, with `scripts/release/release-local.sh <version>` (build,
sign, notarize, publish). The GitHub workflow does the same on a `macos-26` runner but
is manual-dispatch only: that runner bills at 10x and this account's Actions credit
refreshes monthly, so every tag-triggered run from v0.1.0-alpha.15 on was rejected
before it started. Either path builds Pulp at the commit pinned in `pulp.ref` (the
script refuses to run if `../pulp` has drifted) — bump that file, and push pulp main,
whenever a release should pick up new Pulp work. Full checklist:
`scripts/release/README.md`.

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

Plain `cargo` is correct for the Rust crates. For the macOS app (run from `apple/`):

```bash
apple/build-kiemkit.sh            # regenerate the embedded core (REQUIRED after any kiem-ffi/kiem-core change)
cd apple && xcodegen generate     # regenerate Kiem.xcodeproj after editing project.yml
xcodebuild -project Kiem.xcodeproj -scheme Kiem build | xcsift
```

A pre-build script phase fails the Xcode build if `crates/*/src/*.rs` is newer than the
embedded `apple/KiemKit/` — so a stale core can no longer silently clobber metadata; you
get a red build error pointing at `build-kiemkit.sh` instead.

## Architecture

**Cargo workspace** (`Cargo.toml`) with member crates under `crates/`: `kiem-core`
(library), `kiem-cli` (binary + sync daemon), and `kiem-ffi` (UniFFI cdylib bridged to
Swift as the gitignored `apple/KiemKit/` package). The macOS app links KiemKit and the
sibling Pulp editor package.

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
`../pulp/Tests/PulpTests/Fixtures/`, run both test suites, **and rebuild the app's
embedded core with `apple/build-kiemkit.sh`** (a plain `xcodebuild` does NOT regenerate
the gitignored `apple/KiemKit/` XCFramework, so the app keeps running the old rule and
silently clobbers metadata when a note is opened — see
`docs/solutions/integration-issues/stale-prebuilt-kiemkit-xcframework-clobbers-tags-2026-06-20.md`).
There is deliberately **no cross-repo filesystem test** here — each repo tests its own
copy; keeping them in sync is a release/CI concern.

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
- `docs/solutions/` — documented solutions to past problems (bugs, architecture/design
  patterns), organized by category with YAML frontmatter (`module`, `tags`, `problem_type`).
  Relevant when implementing or debugging in documented areas (incl. Pulp rendering).
- Dated doc filenames keep their historical `pear-` prefix; new files use `kiem`.
