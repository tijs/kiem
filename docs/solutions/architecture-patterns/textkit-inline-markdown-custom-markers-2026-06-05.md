---
title: "Rendering markdown inline in a TextKit NSTextView: custom-drawn markers and marker-shrinking"
date: 2026-06-05
category: architecture-patterns
module: pulp
problem_type: architecture_pattern
component: tooling
severity: medium
related_components:
  - documentation
  - testing_framework
applies_when:
  - "Rendering markdown inline in a TextKit NSTextView/UITextView that keeps source in the text storage"
  - "Drawing list markers (bullets, checkboxes) that must align with the styler's text indentation"
  - "Hiding markdown syntax by shrinking its font rather than deleting it"
  - "An inline transform (autolink, emphasis) runs document-wide and must skip definition/code regions"
  - "Verifying custom-drawn (non-glyph) UI that synthetic clicks cannot reach"
  - "A demo/app target is excluded from swift build and only compiles under xcodebuild"
tags:
  - textkit
  - nstextview
  - markdown-rendering
  - list-indentation
  - visual-verification
  - swift-package
---

# Rendering markdown inline in a TextKit NSTextView: custom-drawn markers and marker-shrinking

## Context

Pulp (the editor in `pulp/`, consumed by Kiem) is a Bear-style Markdown editor on
TextKit 1 / `NSTextView`. Unlike a preview pane it keeps the **full Markdown source
in the text storage at all times** and renders inline via a three-stage pipeline:

1. **Tokenize** the source (headings, lists, links, math, code, footnotes, …).
2. **Map tokens → `NSAttributedString` style runs** in `MarkdownStyler` — fonts,
   colors, baseline offsets, paragraph indents.
3. **Custom-draw** what attributes can't express — list bullets, checkboxes,
   code/table backgrounds — in `PulpInternalTextView` drawing overrides.

Syntax markers (`*`, `$$`, `[ ]`, `[^1]`, `][ref]`) are not deleted; they are
**"shrunk"** to a near-zero font so the source stays round-trippable but the markers
are invisible until the cursor lands on the line (selection-aware reveal).

A session fixing six rendering bugs against Bear as the reference surfaced one
recurring failure mode: **two code paths (styler and custom drawer) computing the
same geometry from different constants**, and **inline parsers running over regions
they should have excluded**.

## Guidance

**1. Share geometry between the styler and the custom drawer — never hardcode it twice.**
Bullets/checkboxes were drawn at a hardcoded x (`containerOrigin.x + 14`), so nested
list *text* indented (the styler set `paragraph.headIndent`) while the *glyph* stayed
flush-left. Expose the styler's indent math and have the drawer consume the same
function:

```swift
// MarkdownStyler — promoted to static + internal so the drawer shares it
static let listIndentStep: CGFloat = 24
static func listIndent(depth: Int) -> CGFloat {
    listBaseIndent + CGFloat(max(0, depth)) * listIndentStep
}
```
```swift
// glyph drawing — position from the SAME indent the styler used for the text
let x = containerOrigin.x + MarkdownStyler.listIndent(depth: token.indentDepth)
          - (glyphSize + gap)
```
Cycle the bullet shape by depth like Bear (`NSBezierPath`): `depth % 3` → filled dot
→ hollow ring (stroked inset oval) → filled diamond.

**2. Shrink the full hiding span, not just the punctuation.** Shrinking only `[` and
`]` of `[text][ref]` leaks the ref label ("the Swift forumsforums"). Shrink the whole
`][ref]` tail. For block constructs that own their lines (`$$…$$`), shrink the
**entire delimiter line including its newline** via `NSString.lineRange`, and
**clip to the token range** so the closing marker doesn't bleed into the next
paragraph:

```swift
let ns = source as NSString
let openLine = NSIntersectionRange(ns.lineRange(for: openDelim), token.range)
let closeLine = NSIntersectionRange(ns.lineRange(for: closeDelim), token.range)
```
Special-case the single-line form `$$a=b$$` (open line == close line): shrink only the
`$$` characters, otherwise shrinking the whole line vaporizes the content too.

**3. Exclude definition / non-rendered regions from the inline parser.** The inline
pass ran over the whole document, so `[ref]: https://…` and `[^id]: …` definition
bodies got autolinked. After the block pass, collect `.linkDefinition` /
`.footnoteDefinition` token ranges into an **exclusion set** the inline parser skips,
so definitions render as plain secondary text (Bear behavior). Inline parsers are
greedy by default — "render links everywhere" silently mangles the very definitions
that make reference links work.

**4. Match each construct's visual convention; don't reuse the nearest font.** Inline
`$x$` reused the monospace code font on a code background and was indistinguishable
from `` `code` ``. Render math **italic + accent, no monospace, no background**.
Footnote `[^1]` renders as a **raised superscript** (`.baselineOffset`) in the accent
color, not literal text — done by splitting the regex into capture groups (`[^`, id,
`]`) and shrinking the brackets, leaving the id; the definition shrinks `[^` and `]`
to read "id: body".

**5. Drop obscure constructs rather than half-supporting them.** Setext headings
(`===`/`---` underlines) were removed entirely — Bear ignores them and they collide
with horizontal rules. Removal meant deleting the token case, parse pass, consumed-line
plumbing, styler cases, tests, and demo/spec refs; `---` now cleanly renders as a
horizontal rule.

**6. Verify custom-drawn cells with headless test seams plus screenshots — synthetic
clicks can't hit them.** Accessibility/synthetic clicks land on glyphs the layout
manager owns, not on cells you custom-draw, so they can't assert drawing. Expose
drawing state through test-only accessors and assert geometry **origin-independently**
(deltas, not absolute coordinates, which survive window/scroll changes):

```swift
let bullets = view.bulletItemsForTesting        // test seam over drawingInfo
#expect(abs((bullets[1].rect.minX - bullets[0].rect.minX) - MarkdownStyler.listIndentStep) < 0.5)
```
Pair with a real screenshot. When the peekaboo MCP capture bridge fails ("user
declined TCCs" / "No displays" despite Screen Recording being granted — a stale
cached check, often an npx-vs-homebrew binary mismatch), native
`screencapture -x -R<x,y,w,h> out.png` is the reliable fallback.

## Why This Matters

- **One source of truth for geometry kills an entire bug class.** Every list-indent
  bug traced to the same number living in two places. Once drawer and styler share
  `listIndent(depth:)`, drift between glyph and text becomes unrepresentable.
- **Shrink-not-delete keeps the document the source of truth** (round-trippable,
  cursor-revealable) — but every shrink range is a correctness obligation: too narrow
  leaks raw syntax, mis-clipped corrupts the next paragraph, too wide vaporizes content.
- **Custom-drawn UI is invisible to normal automation.** Relying on synthetic clicks
  gives false passes/failures while never touching the drawn output. Headless geometry
  seams + screenshots are the only reliable signal.
- **Build-graph blind spots ship broken demos.** `swift build`/`swift test` stayed
  green while the `PulpDemoApp` Xcode project (a sibling dir, *not* a SwiftPM target,
  built only by `xcodebuild`) failed to compile after a Pear→Kiem rename. A green core
  build is not evidence the demo works — build app targets explicitly.

## When to Apply

- Building any inline/WYSIWYG-ish Markdown renderer on TextKit that keeps source in
  the text storage rather than a separate preview.
- Whenever the same layout metric (indent, gutter, marker offset) is needed by both
  attribute styling and custom `draw…` overrides — share one function.
- Whenever you hide syntax by shrinking/recoloring rather than deleting — audit the
  exact span and clip it to the token range.
- Whenever an inline transform runs document-wide — define an exclusion set for regions
  it must not touch (definitions, code, frontmatter).
- Deciding whether to support a niche construct: check the reference app (Bear); if it
  ignores it and it conflicts with a common construct, drop it fully.
- Verifying anything custom-drawn into a text view, in CI or locally.
- Any multi-target repo where some targets are excluded from the default build tool
  (SwiftPM excludes Xcode-only app targets) — build those explicitly before trusting them.

## Examples

The six concrete fixes that produced the Guidance above (see the Pulp commit history
for each): nested-list marker alignment + depth glyph cycle; inline math italic-vs-code;
block-math whole-line clip with the single-line special case; reference-link tail shrink
+ definition exclusion; footnote superscript; setext removal. Each follows the matching
numbered point in Guidance.

## Related

- [docs/specs/markdown-format.md](../../specs/markdown-format.md) — the RENDER-ONLY
  rendering-grammar contract this technique implements (marker-shrinking, depth-proportional
  indent sections).
- [docs/plans/2026-05-29-001-feat-pulp-markdown-coverage-plan.md](../../plans/2026-05-29-001-feat-pulp-markdown-coverage-plan.md)
  — the plan that anticipated the custom-drawn-bullet-indent and marker-shrinking risks
  this doc resolves.
- [cross-language-parity-shared-fixtures-2026-05-30.md](cross-language-parity-shared-fixtures-2026-05-30.md)
  — sibling Pulp architecture-pattern doc (different concern: Rust/Swift parity).
