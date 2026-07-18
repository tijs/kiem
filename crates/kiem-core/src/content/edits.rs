//! Body-editing utilities: todo items, checkbox toggling, line-range
//! replacement, and the minimal scalar-indexed splice. Split out of
//! `content/mod.rs` (file-size limit). These are Kiem-side editing helpers,
//! not part of the cross-language derivation contract mirrored by Pulp —
//! though they share its lexical rules via `super::` helpers.

use super::{
    excluded_code_ranges, is_horizontal_ws, overlaps_any, parse_checkbox_line, tag_regex,
    trim_horizontal_ws,
};

/// A Markdown task-list item parsed from a note body. `index` is its 0-based
/// position among all checkbox lines in the body — its stable address for
/// [`set_todo_checked`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub index: usize,
    pub text: String,
    pub checked: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TodoError {
    #[error("todo index {index} out of range ({count} item(s) in note)")]
    IndexOutOfRange { index: usize, count: usize },
}

/// Extract task-list items (`- [ ]` / `- [x]`) from the body, in document order.
/// Matches the same loose dash-bullet convention as [`has_unchecked_todos`].
pub fn extract_todo_items(body: &str) -> Vec<TodoItem> {
    let mut items = Vec::new();
    for line in body.split('\n') {
        if let Some((_, checked, text)) = parse_checkbox_line(line) {
            items.push(TodoItem {
                index: items.len(),
                text,
                checked,
            });
        }
    }
    items
}

/// Walk `body`'s lines and rewrite the checkbox line at `index` with
/// `rewrite(line, pos)`, where `pos` is the byte offset of the state char.
/// Preserves all other text exactly (including line endings). Errors when
/// `index` addresses no checkbox.
fn rewrite_todo_line(
    body: &str,
    index: usize,
    rewrite: impl Fn(&str, usize) -> String,
) -> Result<String, TodoError> {
    let mut seen = 0usize;
    let mut found = false;
    let mut out: Vec<String> = Vec::new();
    for line in body.split('\n') {
        match parse_checkbox_line(line) {
            Some((pos, _, _)) if seen == index => {
                out.push(rewrite(line, pos));
                found = true;
                seen += 1;
            }
            Some(_) => {
                out.push(line.to_string());
                seen += 1;
            }
            None => out.push(line.to_string()),
        }
    }
    if found {
        Ok(out.join("\n"))
    } else {
        Err(TodoError::IndexOutOfRange { index, count: seen })
    }
}

/// Return a copy of `body` with the checkbox at `index` set to `checked`.
pub fn set_todo_checked(body: &str, index: usize, checked: bool) -> Result<String, TodoError> {
    rewrite_todo_line(body, index, |line, pos| {
        let mut new_line = line.to_string();
        new_line.replace_range(pos..pos + 1, if checked { "x" } else { " " });
        new_line
    })
}

/// Return a copy of `body` with the text of the checkbox at `index` replaced,
/// preserving indentation, checked state, and line endings (text normalizes to
/// a single space after the marker). Errors when `index` addresses no checkbox.
pub fn set_todo_text(body: &str, index: usize, text: &str) -> Result<String, TodoError> {
    // One line must stay one line: a line terminator in the new text would
    // splice extra lines (even new checkboxes) into the body and shift every
    // later todo index, so collapse terminators to spaces before trimming.
    let text = text.replace(['\r', '\n'], " ");
    let text = trim_horizontal_ws(&text);
    rewrite_todo_line(body, index, |line, pos| {
        // `pos` is the byte offset of the state char; `]` follows it. Rebuild
        // as "<indent>- [x] <text>", re-appending the `\r` the parser
        // tolerated on CRLF input.
        let content = line.strip_suffix('\r').unwrap_or(line);
        let cr = if content.len() < line.len() { "\r" } else { "" };
        format!("{} {text}{cr}", &content[..pos + 2])
    })
}

/// Return a copy of `body` with a new unchecked task-list item appended,
/// placed immediately after the last existing checkbox line so todos stay
/// grouped (or at the end of the body when there are none). A leading checkbox
/// marker on `text` (e.g. `- [ ] foo`) is tolerated and stripped, so callers
/// can pass either raw text or a full item line.
pub fn append_todo(body: &str, text: &str) -> String {
    let text = match parse_checkbox_line(text) {
        Some((_, _, inner)) => inner,
        None => trim_horizontal_ws(text).to_string(),
    };
    let item = format!("- [ ] {text}");
    let mut lines: Vec<String> = body.split('\n').map(str::to_string).collect();
    match lines.iter().rposition(|l| parse_checkbox_line(l).is_some()) {
        Some(i) => lines.insert(i + 1, item),
        None if body.is_empty() => return item,
        None => return format!("{}\n{item}", body.trim_end_matches('\n')),
    }
    lines.join("\n")
}

/// Remove every parsed occurrence of `tag` while leaving lookalikes and tags in
/// code untouched. A standalone tag line is removed whole; inline removal also
/// consumes one adjacent space so prose does not gain a double space.
pub fn remove_tag(body: &str, tag: &str) -> String {
    let excluded = excluded_code_ranges(body);
    let mut ranges = Vec::new();
    for captures in tag_regex().captures_iter(body) {
        let (Some(whole), Some(name)) = (captures.get(0), captures.get(1)) else {
            continue;
        };
        if name.as_str() != tag {
            continue;
        }
        let preceded_ok = body[..whole.start()]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        if !preceded_ok || overlaps_any(whole.start(), whole.end(), &excluded) {
            continue;
        }

        let line_start = body[..whole.start()].rfind('\n').map_or(0, |i| i + 1);
        let line_end = body[whole.end()..]
            .find('\n')
            .map_or(body.len(), |i| whole.end() + i);
        let line = body[line_start..line_end]
            .strip_suffix('\r')
            .unwrap_or(&body[line_start..line_end]);
        let (mut start, mut end) = (whole.start(), whole.end());
        if line.trim_matches(is_horizontal_ws) == whole.as_str() {
            start = line_start;
            end = (line_end + usize::from(line_end < body.len())).min(body.len());
        } else if let Some(c) = body[end..].chars().next().filter(|c| is_horizontal_ws(*c)) {
            end += c.len_utf8();
        } else if let Some(c) = body[..start]
            .chars()
            .next_back()
            .filter(|c| is_horizontal_ws(*c))
        {
            start -= c.len_utf8();
        }
        ranges.push((start, end));
    }

    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some((_, previous_end)) if start <= *previous_end => {
                *previous_end = (*previous_end).max(end);
            }
            _ => merged.push((start, end)),
        }
    }
    let mut out = body.to_string();
    for (start, end) in merged.into_iter().rev() {
        out.replace_range(start..end, "");
    }
    out
}

/// A text edit expressed in **Unicode scalar** units (`char` counts), the unit
/// Automerge's text sequence indexes by. Deliberately *not* bytes: feeding byte
/// offsets to Automerge corrupts any text past a multi-byte character (the
/// autosurgeon `Text::update` bug this replaces).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodySplice {
    /// Start position, counted in `char`s from the beginning of the body.
    pub pos: usize,
    /// Number of `char`s to delete at `pos`.
    pub del: usize,
    /// Replacement text inserted at `pos`.
    pub insert: String,
}

/// The single minimal splice turning `old` into `new`, trimming the common
/// leading and trailing scalars so unchanged text (and its CRDT history) is
/// preserved. Returns `None` when the bodies are identical.
pub fn body_splice(old: &str, new: &str) -> Option<BodySplice> {
    if old == new {
        return None;
    }
    let o: Vec<char> = old.chars().collect();
    let n: Vec<char> = new.chars().collect();
    let max_pre = o.len().min(n.len());
    let mut pre = 0;
    while pre < max_pre && o[pre] == n[pre] {
        pre += 1;
    }
    let max_suf = max_pre - pre;
    let mut suf = 0;
    while suf < max_suf && o[o.len() - 1 - suf] == n[n.len() - 1 - suf] {
        suf += 1;
    }
    Some(BodySplice {
        pos: pre,
        del: o.len() - pre - suf,
        insert: n[pre..n.len() - suf].iter().collect(),
    })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LineError {
    #[error("line range {start}..={end} is out of range (note has {count} line(s))")]
    OutOfRange {
        start: usize,
        end: usize,
        count: usize,
    },
    #[error("line range {start}..={end} is inverted")]
    Inverted { start: usize, end: usize },
}

/// Replace the 1-based inclusive line range `start..=end` of `body` with
/// `replacement` (which may be empty to delete the lines, or span several
/// lines). Line splitting is on `\n`; a trailing newline is preserved.
pub fn replace_lines(
    body: &str,
    start: usize,
    end: usize,
    replacement: &str,
) -> Result<String, LineError> {
    if start == 0 || end == 0 || start > end {
        return Err(LineError::Inverted { start, end });
    }
    let lines: Vec<&str> = body.split('\n').collect();
    if end > lines.len() {
        return Err(LineError::OutOfRange {
            start,
            end,
            count: lines.len(),
        });
    }
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    out.extend_from_slice(&lines[..start - 1]);
    // Empty replacement deletes the range; otherwise it splices its own lines in.
    if !replacement.is_empty() {
        out.extend(replacement.split('\n'));
    }
    out.extend_from_slice(&lines[end..]);
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_items_reports_index_text_and_state() {
        let body = "# T\n- [ ] first\n- [x] second\nprose\n  - [ ] indented #tag";
        let items = extract_todo_items(body);
        assert_eq!(items.len(), 3);
        assert_eq!(
            items[0],
            TodoItem {
                index: 0,
                text: "first".into(),
                checked: false
            }
        );
        assert_eq!(
            items[1],
            TodoItem {
                index: 1,
                text: "second".into(),
                checked: true
            }
        );
        // Indented item keeps its index; inline text (incl. #tag) preserved.
        assert_eq!(
            items[2],
            TodoItem {
                index: 2,
                text: "indented #tag".into(),
                checked: false
            }
        );
    }

    #[test]
    fn no_checkboxes_yields_empty_and_out_of_range() {
        assert!(extract_todo_items("just prose\n# Heading").is_empty());
        assert_eq!(
            set_todo_checked("just prose", 0, true),
            Err(TodoError::IndexOutOfRange { index: 0, count: 0 })
        );
    }

    #[test]
    fn set_checked_flips_only_the_addressed_item() {
        let body = "- [ ] a\n- [ ] b\n- [ ] c";
        let out = set_todo_checked(body, 1, true).unwrap();
        assert_eq!(out, "- [ ] a\n- [x] b\n- [ ] c");
        let items = extract_todo_items(&out);
        assert_eq!(
            (items[0].checked, items[1].checked, items[2].checked),
            (false, true, false)
        );
    }

    #[test]
    fn uppercase_x_parsed_and_crlf_preserved() {
        let body = "- [X] done\r\n- [ ] todo\r\n";
        let items = extract_todo_items(body);
        assert_eq!((items[0].checked, items[1].checked), (true, false));
        // Toggling preserves CRLF line endings.
        let out = set_todo_checked(body, 1, true).unwrap();
        assert_eq!(out, "- [X] done\r\n- [x] todo\r\n");
    }

    #[test]
    fn non_ascii_lines_do_not_panic_the_checkbox_parser() {
        // A line starting with a multibyte char must not panic the byte-offset
        // parser; checkbox items on other lines still parse.
        let body = "😀 mood\nnaïve note\n- [ ] real task\n我的笔记";
        let items = extract_todo_items(body);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "real task");
        // A non-ASCII checkbox text is preserved.
        let out = set_todo_checked("- [ ] café ☕", 0, true).unwrap();
        assert_eq!(out, "- [x] café ☕");
    }

    #[test]
    fn remove_tag_is_exact_and_code_aware() {
        let body = "# T\ntext #work here\n#work\n#work #work\n`#work`\n```\n#work\n```\n#worker";
        let out = remove_tag(body, "work");
        assert_eq!(out, "# T\ntext here\n\n`#work`\n```\n#work\n```\n#worker");
        assert_eq!(super::super::extract_tags(&out), vec!["worker"]);
    }

    #[test]
    fn remove_tag_handles_a_final_crlf_line() {
        assert_eq!(remove_tag("body\r\n#proj/x", "proj/x"), "body\r\n");
    }

    #[test]
    fn body_splice_positions_are_scalar_counts_not_bytes() {
        // The whole point: positions must count chars, so a multi-byte prefix
        // does not shift them (the autosurgeon byte-offset bug).
        let s = body_splice("café ☕ alpha\nbeta", "café ☕ ALPHA\nbeta").unwrap();
        assert_eq!(s.pos, 7, "7 scalars: 'café ☕ ' — not 9 bytes");
        assert_eq!(s.del, 5); // "alpha"
        assert_eq!(s.insert, "ALPHA");
        assert_eq!(body_splice("same", "same"), None);
        // Applying the splice by CHAR index reproduces `new`.
        let mut chars: Vec<char> = "café ☕ alpha\nbeta".chars().collect();
        chars.splice(s.pos..s.pos + s.del, s.insert.chars());
        assert_eq!(chars.into_iter().collect::<String>(), "café ☕ ALPHA\nbeta");
    }

    #[test]
    fn replace_lines_replaces_deletes_and_validates() {
        let body = "# T\n- [ ] a\n- [ ] b\n- [ ] c";
        assert_eq!(
            replace_lines(body, 3, 3, "- [x] b").unwrap(),
            "# T\n- [ ] a\n- [x] b\n- [ ] c"
        );
        // Multi-line replacement.
        assert_eq!(
            replace_lines(body, 2, 2, "- [ ] a1\n- [ ] a2").unwrap(),
            "# T\n- [ ] a1\n- [ ] a2\n- [ ] b\n- [ ] c"
        );
        // Empty replacement deletes the range.
        assert_eq!(replace_lines(body, 2, 3, "").unwrap(), "# T\n- [ ] c");
        assert_eq!(
            replace_lines(body, 5, 5, "x"),
            Err(LineError::OutOfRange {
                start: 5,
                end: 5,
                count: 4
            })
        );
        assert_eq!(
            replace_lines(body, 0, 1, "x"),
            Err(LineError::Inverted { start: 0, end: 1 })
        );
    }

    #[test]
    fn set_checked_index_beyond_count_errors_unchanged() {
        let body = "- [ ] only";
        assert_eq!(
            set_todo_checked(body, 3, true),
            Err(TodoError::IndexOutOfRange { index: 3, count: 1 })
        );
    }

    #[test]
    fn set_text_replaces_only_the_addressed_item_preserving_state() {
        let body = "# T\n- [ ] a\n  - [x] b\n- [ ] c\r\nprose";
        let out = set_todo_text(body, 1, "  B renamed ").unwrap();
        assert_eq!(out, "# T\n- [ ] a\n  - [x] B renamed\n- [ ] c\r\nprose");
        // CRLF item keeps its line ending and unchecked state; non-ASCII text survives.
        let out = set_todo_text(&out, 2, "café ☕").unwrap();
        assert_eq!(
            out,
            "# T\n- [ ] a\n  - [x] B renamed\n- [ ] café ☕\r\nprose"
        );
        assert_eq!(
            set_todo_text(body, 5, "x"),
            Err(TodoError::IndexOutOfRange { index: 5, count: 3 })
        );
        // Line terminators in the new text collapse to spaces — the item stays
        // one line, so no new checkbox appears and later indices don't shift.
        let out = set_todo_text("- [ ] a\n- [ ] b", 0, "x\r\n- [ ] injected").unwrap();
        assert_eq!(out, "- [ ] x  - [ ] injected\n- [ ] b");
        assert_eq!(extract_todo_items(&out).len(), 2);
    }

    #[test]
    fn append_todo_inserts_after_last_checkbox_keeping_trailing_tags() {
        // New item lands after the last checkbox (end of the list), not after a
        // trailing tag block — so it renders as a real todo, grouped with the rest.
        let body = "## Roadmap\n- [ ] one\n- [ ] two\n\n#proj/kiem_app";
        let out = append_todo(body, "three");
        assert_eq!(
            out,
            "## Roadmap\n- [ ] one\n- [ ] two\n- [ ] three\n\n#proj/kiem_app"
        );
        // A new checkbox is addressable as the next index.
        let items = extract_todo_items(&out);
        assert_eq!(items.len(), 3);
        assert_eq!(
            items[2],
            TodoItem {
                index: 2,
                text: "three".into(),
                checked: false
            }
        );
    }

    #[test]
    fn append_todo_strips_a_redundant_checkbox_marker_and_handles_no_list() {
        // Caller passed a full item line — must not become "- [ ] - [ ] x".
        assert_eq!(append_todo("- [ ] a", "- [ ] b"), "- [ ] a\n- [ ] b");
        // No existing checkbox: append at end of prose, exactly one new line.
        assert_eq!(
            append_todo("just notes", "first"),
            "just notes\n- [ ] first"
        );
        assert_eq!(append_todo("", "first"), "- [ ] first");
    }
}
