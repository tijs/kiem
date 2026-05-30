# Shared fixtures

Language-neutral test contracts shared across Pear implementations.

## `content-derivation.json`

The **canonical** encoding of the title / tags / unchecked-todo derivation rules —
the single source of truth for note metadata. Each case is
`{ name, input, title, tags, hasUncheckedTodos }`.

Run by both implementations as tests:
- `crates/pear-core/tests/content_fixtures.rs` (Rust authority)
- `pulp/Tests/PulpTests/ContentFixtureTests.swift` (Swift mirror, via a vendored
  byte-identical copy)

`pear-core`'s reconciliation test fails if the vendored Pulp copy drifts from this
file. To change a rule: edit both implementations, edit this file, re-vendor the
copy into `pulp/Tests/PulpTests/Fixtures/`, and run both suites.

The human-readable spec these fixtures encode is
[`docs/specs/markdown-format.md`](../docs/specs/markdown-format.md).
