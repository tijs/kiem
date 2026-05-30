# Shared fixtures

Language-neutral test contracts shared across Kiem implementations.

## `content-derivation.json`

The **canonical** encoding of the title / tags / unchecked-todo derivation rules —
the single source of truth for note metadata. Each case is
`{ name, input, title, tags, hasUncheckedTodos }`.

Run by both implementations as tests:
- `crates/kiem-core/tests/content_fixtures.rs` (Rust authority)
- `pulp/Tests/PulpTests/ContentFixtureTests.swift` (Swift mirror, via a vendored
  byte-identical copy in its own repo)

Each repo tests its own implementation against its own copy of the contract. Keeping
the two copies byte-identical is a release/CI concern (a sync step + a CI diff), not
an in-source cross-repo test. To change a rule: edit both implementations, edit this
file, re-vendor the copy into `pulp/Tests/PulpTests/Fixtures/`, and run both suites.

The human-readable spec these fixtures encode is
[`docs/specs/markdown-format.md`](../docs/specs/markdown-format.md).
