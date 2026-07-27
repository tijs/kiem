//! Shared output and input shaping for every command module.
//!
//! Two output modes throughout: `--json` for agents (a single JSON value on
//! stdout, nothing else) and a human-readable default. Anything that is
//! commentary rather than result goes to stderr, so piping `--json` stays
//! clean.
//!
//! `bulk.rs` and `project.rs` predate this module and still print their own
//! JSON inline; unifying them is worth doing, but as its own change.

use std::io::{IsTerminal, Read};

use anyhow::{bail, Result};
use kiem_core::note::NoteMetadata;

/// Body from explicit flags and/or stdin. `--title` prepends an H1 so the
/// derived title matches what the user asked for.
pub fn compose_body(title: Option<String>, body: Option<String>) -> Result<String> {
    let body = body.or_else(read_stdin);
    match (title, body) {
        (Some(t), Some(b)) => Ok(format!("# {t}\n\n{b}")),
        (Some(t), None) => Ok(format!("# {t}")),
        (None, Some(b)) => Ok(b),
        (None, None) => bail!("provide --body, --title, or pipe content on stdin"),
    }
}

/// Piped stdin as a string, or `None` when stdin is a terminal (so an
/// interactive run doesn't block waiting for input that isn't coming).
pub fn read_stdin() -> Option<String> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut buf = String::new();
    stdin.read_to_string(&mut buf).ok()?;
    let trimmed = buf.trim_end();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// A note's title, or a placeholder — an untitled note still needs a row.
pub fn display_title(m: &NoteMetadata) -> &str {
    if m.title.is_empty() {
        "(untitled)"
    } else {
        &m.title
    }
}

/// `  [tag, tag]` for a list row, or empty when the note has none.
pub fn tag_suffix(m: &NoteMetadata) -> String {
    if m.tags.is_empty() {
        String::new()
    } else {
        format!("  [{}]", m.tags.join(", "))
    }
}

/// Turns the store's `NotFound` into a message naming the id the user typed.
pub fn not_found_context(id: &str) -> impl FnOnce(kiem_core::store::StoreError) -> anyhow::Error + '_ {
    move |e| match e {
        kiem_core::store::StoreError::NotFound(_) => anyhow::anyhow!("note not found: {id}"),
        other => other.into(),
    }
}

pub fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
