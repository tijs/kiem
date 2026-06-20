---
title: "Cross-language logic parity via shared golden fixtures"
date: 2026-05-30
last_updated: 2026-06-20
module: kiem-core
problem_type: architecture_pattern
component: testing_framework
severity: high
category: architecture-patterns
related_components:
  - pulp
  - documentation
applies_when:
  - "The same logic is implemented in two or more languages because of an architecture split (e.g. a Rust core plus a standalone Swift package)"
  - "The implementations must produce identical, observable results but cannot or should not share code"
  - "Porting text/parsing logic across languages with different regex engines or string-index models"
tags:
  - cross-language
  - rust
  - swift
  - test-fixtures
  - parity
  - ffi-boundary
---

## Summary

When the same logic is implemented in two languages because of an architecture split — here a Rust core (`kiem-core`) plus a standalone pure-Swift editor package (Pulp) — "don't duplicate the logic" and "keep the package standalone" are mutually exclusive. The resolution is **one authority + verified parity**: one side is authoritative for anything persisted/synced, and a shared language-neutral fixture file run by *both* test suites makes the two implementations provably equal without sharing code. A reconciliation test guards a vendored copy against drift.

## Context

Kiem's core is Rust (`kiem-core`) — cross-platform and authoritative for anything persisted or synced. The editor is **Pulp**, a standalone pure-Swift package with no Rust dependency, deliberately shippable on its own. Both must derive a note's **title**, **tags**, and **unchecked-todo** state from the same Markdown body text, and both must produce **identical** results — otherwise the same note shows one title in the editor and a different title after a sync round-trip.

The two obvious goals can't both hold:

- *"Don't duplicate the logic"* → would force a single shared implementation.
- *"Pulp stays a standalone Swift package"* → forbids depending on the Rust core (a standalone package can't link a Rust lib).

So the question is not "how do we avoid two implementations" but "how do we keep two implementations provably equal." See [docs/specs/markdown-format.md](../../specs/markdown-format.md) for the current derivation rule set and [the master plan's shared-logic boundary decision](../../plans/2026-05-24-001-feat-pear-notes-app-plan.md) for why the split exists.

## Guidance

Adopt a **one-authority + verified-parity** pattern with three principles.

**1. Authority — one side owns the truth.**
For any value that is persisted or synced, exactly one implementation is authoritative; the other is a display-only mirror the authoritative side never trusts. In Kiem, Rust is authoritative. The Swift editor derives title/tags locally for instant UI feedback, but those values are never written back — the bridge sends only the raw body and lets Rust re-derive:

```
// Swift editor → core: send the body text, NOT the locally-derived title/tags.
update_note(body)            // Rust derives + persists the authoritative title/tags
```

A drift bug then can never corrupt stored data; at worst the editor shows a transient label the next render reconciles.

**2. Verified parity — share a contract, not code.**
Share a **language-neutral fixture file** pinning inputs to expected outputs. Both test suites load and assert against the same file:

```json
// fixtures/content-derivation.json  (the contract)
[
  { "input": "# Title\n\nbody #tag",
    "title": "Title", "tags": ["tag"], "hasUncheckedTodos": false },
  { "input": "- [ ] do thing",
    "title": "do thing", "tags": [], "hasUncheckedTodos": true }
]
```

```rust
// crates/kiem-core/tests/content_fixtures.rs
for case in load("fixtures/content-derivation.json") {
    assert_eq!(derive_title(&case.input), case.title);
    assert_eq!(extract_tags(&case.input), case.tags);
}
```

```swift
// pulp/Tests/PulpTests/ContentFixtureTests.swift
for case in loadFixtures() {
    #expect(ContentAnalyzer.title(of: case.input) == case.title)
    #expect(ContentAnalyzer.tags(of: case.input) == case.tags)
}
```

Any divergence now fails a test in at least one suite. The two implementations are provably equal on every enumerated case — without sharing a line of code.

**3. Anti-drift — vendor + reconcile.**
Because Pulp is a separate repo and must stay standalone, it can't path-reference the canonical file. It **vendors a byte-identical copy**, and the authoritative repo runs a **reconciliation test** that fails loudly on drift:

```rust
#[test]
fn vendored_fixtures_match_canonical() {
    let canonical = read("fixtures/content-derivation.json");
    let vendored  = read("pulp/Tests/.../content-derivation.json");
    assert_eq!(canonical, vendored, "Pulp fixture copy drifted — re-vendor");
}
```

The change-control rule becomes mechanical: **edit both implementations → edit the canonical fixtures → re-vendor the copy → run both suites.** Skip a step and a test goes red.

### Divergence pitfalls that make naive ports silently wrong

Porting "the same" regex/string logic between languages is where parity quietly breaks. The ones that bit us (all surfaced by an adversarial code review, not by the happy-path suites):

- **Regex engine capability gap.** Rust's `regex` crate has **no lookbehind and no backreferences**; Swift's `NSRegularExpression` has both. Swift's `(?<=\s|^)#tag` lookbehind became an explicit "is the preceding char whitespace / start-of-string?" check in Rust. Swift's `\1` backreference for a fenced-code closing fence became a manual line scan that records the opening fence length and finds an exact-length close.
- **UTF-16 vs UTF-8 offsets.** Swift `NSRange` indexes UTF-16 code units; Rust strings are UTF-8 byte offsets. Emoji/CJK desynchronize naive index math — an off-by-one that only appears on non-ASCII input.
- **Unicode whitespace classes.** Swift `CharacterSet.whitespaces` and regex `\s` match NBSP (U+00A0), ideographic space (U+3000), etc. A naive Rust check like `c == ' ' || c == '\t'` **silently drops** a tag preceded by one of those — producing different tag sets for the same note depending on which language ran. Mirror with a Unicode-aware predicate (`\p{L}`, `\p{N}`, or `char::is_whitespace()` minus line terminators).
- **CRLF handling.** Swift treats `\r\n` as a single grapheme; byte-oriented Rust does not. Normalize `\r\n` → `\n` on **both** sides before processing.
- **Fixtures can enshrine quirks.** Title derivation strips only `# ` (H1), leaving `## Sub` verbatim. The fixture pins this so both stay equal — but in review, distinguish *"the two implementations agree"* from *"this is the desired behavior."* A golden file guarantees the former, not the latter.

## Why This Matters

- **Silent data divergence is the worst failure mode.** Without the shared contract, a whitespace-class mismatch doesn't throw — it produces a different tag set after sync, so a note "loses" a tag on round-trip. These bugs are nearly invisible to single-language testing because each suite passes on its own inputs.
- **It preserves the architecture you actually want.** You keep the Rust core *and* a genuinely standalone Swift package — no collapsing into one artifact, no bolting an FFI dependency onto the editor just to share logic.
- **It makes "the same logic" enforceable, not aspirational.** "We'll keep them in sync manually" always rots. A reconciliation test plus a shared fixture file turns parity into a build gate: drift is a red test, not a production surprise.
- **It scales to more surfaces.** A third implementation (e.g. a TypeScript web client) joins by loading the same fixtures — the contract is already language-neutral.

## When to Apply

Apply when **all** hold:

- The same logic is (or will be) implemented in **two or more languages** due to an architecture split — a shared core plus platform-native UIs, multiple clients, or a service plus an edge reimplementation.
- The implementations must produce **identical, observable results** (not merely "similar enough").
- You **cannot or do not want to share code** across the boundary — one component must stay dependency-free, the languages can't link, or an FFI dependency would defeat the purpose.

Strong additional signals: the logic is **pure** (input → output, cheap to capture as fixtures); the output feeds **persistence or sync** (divergence corrupts data, not just rendering); one component ships **independently** (separate repo / open-source package) so it must vendor a copy.

**Overkill when:** a single codebase/language; logic allowed to differ per platform; or throwaway/non-persisted values where transient divergence is harmless.

## Examples

**Before — "we'll keep them in sync" (the anti-pattern):**

```
kiem-core (Rust):   extract_tags()        →  uses  c == ' ' || c == '\t'
Pulp (Swift):       ContentAnalyzer.tags  →  uses  \s  (matches NBSP, U+3000…)

Test reality:
  cargo test    ✅ green   (Rust suite uses only ASCII-space inputs)
  swift test    ✅ green   (Swift suite uses only ASCII-space inputs)
  production:   "review #idea" with a NBSP before #idea
                → Swift extracts ["idea"], Rust extracts []
                → tag silently lost on sync round-trip; no test caught it
```

**After — shared fixture contract + reconciliation:**

```
kiem-app/fixtures/content-derivation.json   ← canonical (inputs → expected)
        │
        ├── kiem-app/crates/kiem-core/tests/content_fixtures.rs  loads + asserts (own repo)
        ├── pulp/Tests/.../ContentFixtureTests.swift             loads + asserts (vendored copy, own repo)
        └── CI / sync step: vendored copy == canonical (not an in-source cross-repo test)

Add the NBSP case once:
  { "input": "review #idea", "title": "review #idea", "tags": ["idea"], ... }
        │
        ├── Rust suite → RED  (ASCII-only check drops the tag)  → fix Rust predicate
        └── Swift suite → green
  Re-run → both green → parity proven on that case, forever.
```

**Swift → Rust regex translation cheat-sheet (reusable):**

| Swift `NSRegularExpression` | Rust `regex` equivalent |
|---|---|
| `(?<=\s\|^)` lookbehind | explicit "preceding char is whitespace or start" check |
| `\1` backreference (matched fence close) | manual scan: record open length, find exact-length close |
| `\s` / `CharacterSet.whitespaces` | `\p{White_Space}` or `char::is_whitespace()` (mind line terminators) |
| `NSRange` (UTF-16 units) | byte offsets (UTF-8) — convert deliberately, never assume 1:1 |
| `\r\n` as one grapheme | normalize `\r\n` → `\n` before processing |

**Change-control workflow it produces:**

```
To change any derivation rule:
  1. edit Rust implementation
  2. edit Swift implementation
  3. edit canonical fixtures/content-derivation.json
  4. re-run the sync step to re-vendor the copy into Pulp
  5. run cargo test (in kiem-app) && swift test (in pulp)  — both green
  6. CI diffs the two copies (the cross-repo check lives here, not in-source)
Skip a code/fixture step → that repo's suite goes red. Skip the sync → CI goes red.
```

**Blind spot — compiled/vendored artifacts.** This workflow keeps two *source*
implementations in lockstep, verified by tests. It does **not** catch a stale *binary*
that embeds one of them: the macOS app links a prebuilt `KiemKit` XCFramework, and
`xcodebuild` won't regenerate it, so every source and fixture test can be green while the
app runs a months-old copy of the rule and silently clobbers data on note open. Add
"rebuild the embedded artifact (`apple/build-kiemkit.sh`)" as a step whenever a
derivation rule changes — see the Related doc below.

## Related

- [docs/specs/markdown-format.md](../../specs/markdown-format.md) — the canonical, living spec of the derivation rules and the RENDER-ONLY vs NORMATIVE contract this pattern enforces.
- [docs/plans/2026-05-24-001-feat-pear-notes-app-plan.md](../../plans/2026-05-24-001-feat-pear-notes-app-plan.md) — the Rust-core/Swift-UI architecture split and the Tier-based shared-logic boundary decision that motivated this pattern.
- [Stale prebuilt KiemKit XCFramework clobbers note tags on open](../integration-issues/stale-prebuilt-kiemkit-xcframework-clobbers-tags-2026-06-20.md) — the *source-to-artifact* counterpart to this *source-to-source* contract: a real instance of the binary blind spot above, where every parity test was green but the app's embedded core was stale.
