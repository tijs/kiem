---
title: "Kiem/Pulp Markdown format contract"
type: spec
status: living
updated: 2026-05-29
---

# Kiem/Pulp Markdown format contract

The exact Markdown subset that Pulp renders and that Kiem derives metadata from.
This document is the human-readable spec; the **derivation rules** (title, tags,
unchecked todos) additionally have an executable contract in
[`fixtures/content-derivation.json`](../../fixtures/content-derivation.json),
which both implementations run as tests.

It is GitHub-Flavored-Markdown-ish, deliberately narrowed. It is **not** CommonMark
and does not aim to be — it is the set of forms Pulp tokenizes for inline rendering
plus the forms Kiem indexes.

## Implementations

Two implementations exist today; both must agree on everything marked
**NORMATIVE** below.

| Concern | Implementation | Language | Role |
|---|---|---|---|
| Inline rendering grammar (tokens) | `MarkdownTokenizer` / `MarkdownStyler` | Swift (Pulp) | Single implementation — Kiem embeds Pulp, does not re-tokenize |
| Title / tags / todos derivation | `ContentAnalyzer` | Swift (Pulp) | Mirror — convenience for standalone Pulp |
| Title / tags / todos derivation | `kiem_core::content` | Rust (Kiem) | **Authority** — what Kiem persists, indexes, and syncs |

**Source-of-truth rule:** for any persisted or synced value (the note's
`metadata.title` and `metadata.tags`), the **Rust** derivation is authoritative.
Pulp's Swift derivation is never trusted by Kiem; it exists so Pulp works as a
standalone package. The two are kept in lockstep by the shared fixtures, not by
sharing code (see [the plan's Tier-1 decision](../plans/2026-05-24-001-feat-pear-notes-app-plan.md)).

Markings used here:
- **NORMATIVE** — cross-implementation contract; divergence is a bug, guarded by fixtures.
- **RENDER-ONLY** — Pulp rendering grammar; single Swift implementation, informative.

---

## Block elements (RENDER-ONLY grammar)

Patterns are the actual tokenizer regexes. Block elements are matched per line
unless noted.

| Element | Recognized form | Regex |
|---|---|---|
| Heading (H1–H6) | `#`…`######` + whitespace, then text | `^(#{1,6})\s+` |
| Task item | `- [ ]` / `- [x]` / `- [X]` (leading indent allowed) | `^(\s*- \[)([ xX])(\]\s)` |
| Unordered list item | `-`, `*`, or `+` + space (indent allowed) | `^(\s*[-*+]\s)` |
| Ordered list item | `1.` + space (indent allowed) | `^(\s*)(\d+\.\s)` |
| Blockquote | one or more `>` + whitespace | `^(>+\s)` |
| Horizontal rule | 3+ of `-`, `*`, or `_` on their own line | `^\s*([-*_]){3,}\s*$` |
| Fenced code block | see [Code fences](#code-fences-normative) | `(?m)^(`{3,}\|~{3,})([^\n]*)\n([\s\S]*?)^\1\s*$` |
| Block math | `$$…$$`, may span lines (styled, not typeset) | `(\$\$)([\s\S]+?)(\$\$)` |
| Link definition | `[ref]: url` on its own line | `^(\[)([^\]]+)(\]:\s*)(\S+)\s*$` |
| Footnote definition | `[^id]: text` on its own line | `^(\[\^[^\]]+\]:)\s` |
| Table | see [Tables](#tables-render-only) | row `^\|.+\|\s*$`, separator `^\|?[\s-]*\|[\s:\|-]+\|?\s*$` |

Task items are matched before plain list items so `- [ ]` is never double-counted
as a bullet. **List indentation is depth-proportional**: leading whitespace is read
as nesting depth (2 spaces or 1 tab per level) and the renderer indents each level
further — bullet, ordered, and task items alike. Only ATX headings (`#`…`######`)
are supported; **setext headings** (`===` / `---` underlines) are not — a `---` line
is a horizontal rule and a `| --- |` line is a table separator.

## Inline elements (RENDER-ONLY grammar)

Inline elements are not matched inside code blocks or inline code.

| Element | Form | Regex |
|---|---|---|
| Bold + italic | `***text***` or `___text___` | `(\*{3})(.+?)(\*{3})` (asterisk); underscore variant below |
| Bold | `**text**` or `__text__` | `(\*{2})(.+?)(\*{2})` (asterisk); underscore variant below |
| Italic | `*text*` or `_text_` | `(?<![*])(\*)(?![*])(.+?)(?<![*])(\*)(?![*])` (asterisk); underscore variant below |
| Strikethrough | `~~text~~` | `(~~)(.+?)(~~)` |
| Highlight | `==text==` | `(==)(.+?)(==)` |
| Inline code | `` `code` `` (backtick run, balanced) | `` (`+)(.+?)(\1) `` |
| Inline math | `$…$` (styled, not typeset) | `(?<!\$)(\$)(?=\S)([^$\n]+?)(?<=\S)(\$)(?!\$)` |
| Link | `[text](url)` | `(\[)([^\]]+)(\]\()([^)]+)(\))` |
| Image | `![alt](url)` (recognized + styled; no inline thumbnail) | `(!\[)([^\]]*)(\]\()([^)]+)(\))` |
| Autolink | bare `http(s)://…` URL | `(?<![\w/(])(https?://[^\s<>]+)` |
| Reference link | `[text][ref]` | `(\[)([^\]]+)(\]\[)([^\]]*)(\])` |
| Footnote reference | `[^id]` | `(\[\^[^\]]+\])` |
| Hashtag | `#tag`, `#nested/tag` | see [Tags](#tags-normative) |

**Underscore emphasis** (`_italic_`, `__bold__`, `___bolditalic___`) renders the
same as the asterisk forms, but with **intra-word protection**: an underscore is a
delimiter only when its outer side is start/end-of-text or a non-word character, and
the inner side is non-whitespace. This is the CommonMark "no intraword `_` emphasis"
rule, and it is exactly what keeps `snake_case`, `path/to_file`, and `#v2_release`
from being italicized. Asterisk emphasis keeps its looser rule (it can occur
intra-word). The underscore patterns:

- bold-italic: `(?<![A-Za-z0-9_])(_{3})(?=\S)(.+?)(?<=\S)(_{3})(?![A-Za-z0-9_])`
- bold: `(?<![A-Za-z0-9_])(_{2})(?=\S)(.+?)(?<=\S)(_{2})(?![A-Za-z0-9_])`
- italic: `(?<![A-Za-z0-9_])(_)(?=\S)(.+?)(?<=\S)(_)(?![A-Za-z0-9_])`

**Math** (`$…$`, `$$…$$`) is rendered as a distinct styled span (code font, secondary
tint) — the LaTeX is **not typeset**, and its content is exempt from inline parsing so
`a_{ij}` is not italicized. Inline `$` requires non-space immediately inside the
delimiters, so prose like `it cost $5 and $10` is not treated as math.

**Images** (`![alt](url)`) are recognized and styled but not rendered as inline
thumbnails (deferred). **Reference links** (`[text][ref]` + `[ref]: url`) and
**footnotes** (`[^id]` + `[^id]: …`) are styled with marker-shrinking; their
definitions are not resolved or made clickable (deferred). **Autolinks** trim trailing
sentence punctuation and an unbalanced closing `)`/`]`, and never double-match a URL
that is already the target of a `[text](url)` link.

---

## Derived properties (NORMATIVE)

These three derivations are the cross-implementation contract. Every rule here is
pinned by a fixture; both Rust and Swift must produce identical output.

Both implementations first **normalize line endings** to LF (`\r\n` → `\n`, then
lone `\r` → `\n`) before any other processing. (Swift treats `\r\n` as one
grapheme; normalizing up front keeps the byte-oriented Rust scan and the
grapheme-oriented Swift scan in agreement.)

### Title

The title is **derived from the body**, never edited directly.

1. Scan lines top to bottom. Skip a line if, after trimming horizontal whitespace
   (see [Whitespace](#whitespace-normative)), it is:
   - empty,
   - a table row (starts with `|`), or
   - a divider-only line (every character is one of `-`, `|`, `:`, space).
2. On the first surviving line: if it begins with `# ` (H1 marker — `#` followed by
   a space), strip that marker.
3. Trim horizontal whitespace from the result and return it.
4. If no line survives, the title is the empty string.

**Intentional quirk:** only the H1 marker `# ` is stripped. `## Subheading` and
deeper ATX headings are returned verbatim, markers included. The rendering grammar
recognizes H1–H6 as headings *visually*, but title derivation only unwraps H1. This
is a deliberate narrowing, not a parser gap — if a note's first line is `## Foo`,
its title is `## Foo`.

### Tags (NORMATIVE)

Tags are `#hashtags` parsed from the body. The metadata tag index and the inline
hashtags are the same data: hashtags are the authoring interface, `metadata.tags`
is the queryable denormalization.

- A tag matches `#` followed by an ASCII letter, then ASCII letters/digits/`_`/`/`:
  pattern `#([a-zA-Z][a-zA-Z0-9_/]*)`. The returned tag excludes the leading `#`.
- Nested tags keep their slashes whole: `#work/meetings/2025` → `work/meetings/2025`.
- A `#` is a tag **only if preceded by inline whitespace** (see
  [Whitespace](#whitespace-normative)). A `#` at the start of a line is a heading
  marker, not a tag.
- Tags inside [fenced code blocks](#code-fences-normative) or inline code spans are
  ignored.
- Results are **deduplicated, preserving first-seen order**.

### Unchecked todos (NORMATIVE)

`hasUncheckedTodos` is true iff the body contains the literal substring `- [ ]`
(an unchecked task item). This is a deliberately cheap check — it does not require
the marker to be at a line start and does not exclude code blocks.

---

## Code fences (NORMATIVE)

Fenced code blocks define exclusion regions for tag parsing, so both
implementations must agree on their boundaries. The semantics mirror the Swift
regex `^(`{3,}|~{3,})[^\n]*\n[\s\S]*?^\1\s*$`:

- **Opener:** a line that begins (no leading whitespace) with a run of **3 or more**
  backticks or tildes. An **info string** may follow the run (e.g. ```` ```rust ````,
  `~~~ swift`).
- **Closer:** the first later line that is the **exact same fence run** — same
  character, same length — followed by only whitespace. (The length match is exact:
  a 4-backtick opener is not closed by 3 backticks, and vice versa.)
- **Unterminated:** an opener with no matching closer produces **no** code block —
  it is ignored, and the following lines are parsed normally. (It does not extend to
  end of input.)
- Backtick fences and tilde fences are independent; a `~~~` line never closes a
  ```` ``` ```` block.

Inline code spans (`` `…` ``) are also excluded from tag parsing.

---

## Whitespace (NORMATIVE)

Two whitespace classes matter for the derivations, mirroring the Swift
`NSRegularExpression`/`CharacterSet` semantics that the Rust side reproduces:

- **Horizontal whitespace** (title trimming) = tab `U+0009` plus the Unicode `Zs`
  separator category (space, `U+00A0` NBSP, `U+3000` ideographic space, the
  `U+2000`–`U+200A` range, `U+202F`, `U+205F`, `U+1680`). Matches Swift
  `CharacterSet.whitespaces`. Excludes line terminators and the VT/FF controls.
- **Tag-preceding whitespace** = any Unicode whitespace **except** line terminators
  (`\n`, `\r`, `U+0085`, `U+2028`, `U+2029`). This is broader than horizontal
  whitespace — it additionally accepts VT (`U+000B`) and FF (`U+000C`), matching
  ICU regex `\s` minus the line-start cases. So `word⍽#tag` (NBSP before `#`) yields
  the tag `tag`.

CJK notes routinely use ideographic/full-width spaces, so getting these classes
right is not academic — an ASCII-only check silently dropped real tags before this
contract existed.

---

## The fixture contract

[`fixtures/content-derivation.json`](../../fixtures/content-derivation.json) is the
canonical, language-neutral encoding of the NORMATIVE derivation rules. Each case is
`{ name, input, title, tags, hasUncheckedTodos }`.

- `kiem_core`'s `tests/content_fixtures.rs` runs every case and asserts the Rust
  output matches.
- Pulp's `Tests/PulpTests/ContentFixtureTests.swift` runs every case (parameterized)
  and asserts the Swift output matches.
- Pulp vendors a byte-identical copy of the fixtures (so it stays standalone);
  `kiem_core` has a reconciliation test that the copy has not drifted.

**To change a derivation rule:** edit both implementations, edit the canonical
fixtures, re-vendor the copy into `pulp/Tests/PulpTests/Fixtures/`, and run both test
suites. The reconciliation test fails loudly if the copies diverge.

---

## Not supported (yet)

Tracked in the plan's Pulp remaining-work list, not in this contract:

- **Inline image thumbnails** — `![alt](url)` is recognized and styled, but the image
  is not loaded or drawn inline (deferred).
- **Math typesetting** — `$…$` / `$$…$$` render as styled raw LaTeX, not typeset.
- **Reference-link / footnote resolution** — `[text][ref]`, `[^id]` are styled, but
  definitions are not resolved and markers are not clickable/jump-to.
- **Setext headings** (`===` / `---` underlines) — intentionally unsupported (Bear
  ignores them too); use ATX `#` headings. A `---` line renders as a horizontal rule.
- Definition lists.
- Heading folding, list indentation via Tab (authoring gesture — distinct from the
  depth-proportional *rendering* of already-indented lists, which is supported).

---

## Tables (RENDER-ONLY)

GFM pipe tables, rendered by Pulp via overlay drawing. A table is a header row, a
separator row, then zero or more data rows:

```
| Column 1 | Column 2 |
| --- | --- |
| a | b |
```

- A table starts where a table row (`| … |`) is immediately followed by a separator
  row (`| --- | --- |`, alignment colons `:` permitted).
- Column count is the number of `|`-delimited cells in the header.
- Structural edits rewrite the table to canonical form (`TableEditor`) so the source
  stays CRDT-friendly. Table source is currently Pulp-only (Swift); see the plan's
  Tier-3 note for if/when canonicalization moves to Rust.

Tables are excluded from title derivation (a leading table does not become the
title) but table cell text is otherwise ordinary body text.
