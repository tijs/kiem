//! Content-derivation rules: title, tags, and unchecked-todo detection from a
//! note's Markdown body. These are the canonical definitions; Pulp's Swift
//! `ContentAnalyzer` mirrors them and is checked against the shared fixtures.
//!
//! Note on parity: Rust's `regex` crate supports neither lookbehind nor
//! backreferences, both of which the Swift `NSRegularExpression` rules use. The
//! lookbehind `(?<=\s|^)` is replaced by an explicit preceding-character check,
//! and the backreference-based fenced-code-block match (`^\1\s*$`) is replaced by
//! a line scan that reproduces the same exact-length close semantics. Both sides
//! normalize CRLF to LF first so line-ending quirks (Swift treats `\r\n` as one
//! grapheme) cannot cause divergence. The shared fixture suite guarantees the two
//! implementations agree on every case that matters.

use regex::Regex;
use std::sync::OnceLock;

/// Derive a note title from its body: the first non-empty line that is not a
/// table row or table-separator-like line, with a leading `# ` (H1 marker)
/// stripped. Returns an empty string when the body has no usable line.
///
/// Only the H1 marker `# ` is stripped — `## ` and deeper ATX headings are kept
/// verbatim, matching Swift `ContentAnalyzer`. This is intentional, not a partial
/// heading parser.
pub fn derive_title(body: &str) -> String {
    let normalized = normalize_newlines(body);
    for raw_line in normalized.split('\n') {
        let line = trim_horizontal_ws(raw_line);
        if line.is_empty() {
            continue;
        }
        if line.starts_with('|') {
            continue; // table row
        }
        if line.chars().all(|c| matches!(c, '-' | '|' | ':' | ' ')) {
            continue; // table separator / divider-only line
        }
        let title = line.strip_prefix("# ").unwrap_or(line);
        return trim_horizontal_ws(title).to_string();
    }
    String::new()
}

/// Extract unique `#hashtags` from the body, in first-seen order. Tags inside
/// fenced or inline code are ignored. A tag is `#` immediately followed by a
/// letter, so `# Heading` (a space after `#`) is a heading and never a tag — but
/// a `#tag` at the start of a line or the document still counts. Nested tags
/// (`#work/meetings`) are kept whole. The returned strings exclude the leading `#`.
pub fn extract_tags(body: &str) -> Vec<String> {
    let normalized = normalize_newlines(body);
    let body = normalized.as_str();
    let excluded = excluded_code_ranges(body);
    let mut tags: Vec<String> = Vec::new();

    for caps in tag_regex().captures_iter(body) {
        let whole = caps.get(0).expect("group 0 always present");
        let start = whole.start();

        // Mirror Swift's `(?<=\s|^)#…`: the `#` must be at the start of the
        // string or preceded by whitespace — and a line terminator counts, so a
        // line-start `#tag` is a tag. Headings are already excluded by the regex,
        // which requires a letter right after `#`, so `# Heading` never matches.
        let preceded_ok = match body[..start].chars().next_back() {
            None => true,
            Some(c) => c.is_whitespace(),
        };
        if !preceded_ok {
            continue;
        }
        if overlaps_any(start, whole.end(), &excluded) {
            continue;
        }

        let name = caps
            .get(1)
            .expect("tag capture group present")
            .as_str()
            .to_string();
        if !tags.contains(&name) {
            tags.push(name);
        }
    }
    tags
}

/// Whether the body contains at least one unchecked task item (`- [ ]`).
pub fn has_unchecked_todos(body: &str) -> bool {
    body.contains("- [ ]")
}

// MARK: - Internals

/// Normalize line endings to LF so CRLF / lone-CR input derives identically on
/// both sides of the FFI boundary.
fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Horizontal whitespace matching Swift `CharacterSet.whitespaces`: tab plus the
/// Unicode `Zs` separators (space, NBSP, ideographic space, …). Excludes line
/// terminators and the VT/FF control whitespace that `.whitespaces` omits.
fn is_horizontal_ws(c: char) -> bool {
    c == '\t'
        || (c.is_whitespace()
            && !matches!(
                c,
                '\n' | '\r' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
            ))
}

fn trim_horizontal_ws(s: &str) -> &str {
    s.trim_matches(is_horizontal_ws)
}

fn tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"#([a-zA-Z][a-zA-Z0-9_/]*)").expect("valid tag regex"))
}

fn inline_code_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`[^`]+`").expect("valid inline-code regex"))
}

/// Byte ranges of regions where hashtags must be ignored: fenced code blocks and
/// inline code spans.
fn excluded_code_ranges(body: &str) -> Vec<(usize, usize)> {
    let mut ranges = fenced_code_ranges(body);
    for m in inline_code_regex().find_iter(body) {
        ranges.push((m.start(), m.end()));
    }
    ranges
}

/// Byte ranges spanned by fenced code blocks, mirroring Swift's regex
/// `^(`{3,}|~{3,})[^\n]*\n[\s\S]*?^\1\s*$`:
/// - an opener is a line beginning (no leading whitespace) with a run of 3+
///   backticks or tildes, optionally followed by an info string;
/// - it closes on the first later line that is *exactly* the same fence run
///   (same character, same length) followed only by whitespace (the `\1` back
///   reference is exact-length);
/// - an opener with no matching close produces no range at all (the regex finds
///   no match), so it is skipped rather than extended to end of input.
fn fenced_code_ranges(body: &str) -> Vec<(usize, usize)> {
    let spans: Vec<(usize, &str)> = line_spans(body).collect();
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < spans.len() {
        let Some((ch, len)) = open_fence(spans[i].1) else {
            i += 1;
            continue;
        };
        let block_start = spans[i].0;
        let close = spans[i + 1..]
            .iter()
            .position(|&(_, line)| is_closing_fence(line, ch, len))
            .map(|offset| i + 1 + offset);

        match close {
            Some(j) => {
                let (close_start, close_line) = spans[j];
                ranges.push((block_start, close_start + close_line.len()));
                i = j + 1;
            }
            // No exact-length close: the regex finds no match, so emit no range
            // and resume scanning after the opener.
            None => i += 1,
        }
    }
    ranges
}

/// A fence opener: a line starting with a run of 3+ identical `` ` `` or `~`,
/// with any info string allowed after the run. Returns the fence char and run
/// length.
fn open_fence(line: &str) -> Option<(char, usize)> {
    let fence_char = match line.chars().next() {
        Some(c @ ('`' | '~')) => c,
        _ => return None,
    };
    let run = line.chars().take_while(|&c| c == fence_char).count();
    (run >= 3).then_some((fence_char, run))
}

/// A closing fence: a run of *exactly* `len` of `fence_char`, then only
/// whitespace (mirrors the exact-length `\1` backreference plus `\s*$`).
fn is_closing_fence(line: &str, fence_char: char, len: usize) -> bool {
    let run = line.chars().take_while(|&c| c == fence_char).count();
    run == len && line.chars().skip(len).all(char::is_whitespace)
}

/// Iterate `(byte_offset, line_without_trailing_newline)` over `body`.
fn line_spans(body: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0usize;
    body.split('\n').map(move |line| {
        let start = offset;
        offset += line.len() + 1; // +1 for the '\n' removed by split
        (start, line)
    })
}

fn overlaps_any(start: usize, end: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|&(s, e)| start < e && s < end)
}

#[cfg(test)]
mod tests {
    //! Fast smoke tests. The authoritative cross-language contract lives in
    //! `tests/content_fixtures.rs`; these cover a few non-fixture edge cases
    //! (unterminated fence, tilde fence, CRLF) for quick local feedback.
    use super::*;

    #[test]
    fn title_from_h1() {
        assert_eq!(derive_title("# My Title\nbody"), "My Title");
    }

    #[test]
    fn deeper_heading_not_stripped() {
        assert_eq!(derive_title("## Sub"), "## Sub");
    }

    #[test]
    fn crlf_title_normalized() {
        assert_eq!(derive_title("# T\r\nbody"), "T");
    }

    #[test]
    fn tag_at_line_start_is_extracted() {
        // A `#` + letter at the start of a line/document is a tag (only `# ` with
        // a space is a heading).
        assert_eq!(extract_tags("#nota tag"), vec!["nota"]);
        assert_eq!(extract_tags("intro\n#hello world"), vec!["hello"]);
    }

    #[test]
    fn heading_with_space_is_not_a_tag() {
        assert!(extract_tags("# Heading\nbody").is_empty());
        assert!(extract_tags("## Section").is_empty());
    }

    #[test]
    fn tag_after_nbsp_is_kept() {
        assert_eq!(extract_tags("word\u{00A0}#tag"), vec!["tag"]);
    }

    #[test]
    fn tilde_fence_excludes_tags() {
        assert_eq!(extract_tags("p\n~~~\n#no\n~~~\nreal #yes"), vec!["yes"]);
    }

    #[test]
    fn info_string_fence_excludes_tags() {
        assert_eq!(extract_tags("p\n```rust\nx #no\n```\ndone #yes"), vec!["yes"]);
    }

    #[test]
    fn unterminated_fence_does_not_exclude() {
        assert_eq!(extract_tags("p\n```\ntext #inside\nno close"), vec!["inside"]);
    }

    #[test]
    fn mismatched_fence_chars_do_not_pair() {
        assert_eq!(extract_tags("p\n```\nx #a\n~~~\ny #b"), vec!["a", "b"]);
    }

    #[test]
    fn unchecked_todos() {
        assert!(has_unchecked_todos("- [ ] do it"));
        assert!(!has_unchecked_todos("- [x] done"));
    }
}
