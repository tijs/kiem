//! Project resolution and the committed `.kiem` marker.
//!
//! A project is the reserved tag `proj/<slug>`. A repo declares its project in a
//! small committed `.kiem` file (`project = "proj/<slug>"`) so the binding travels
//! with the repo across machines — Kiem itself stores no filesystem paths. The
//! agent's "current project" resolves from that marker (searched up the directory
//! tree), falling back to the slugified directory name when no marker exists.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Marker filename committed at a project's repo root.
pub const MARKER: &str = ".kiem";
/// Tag namespace that makes a tag a project.
pub const TAG_PREFIX: &str = "proj/";

/// Resolve the current project tag: explicit override → `.kiem` marker (current
/// dir or any ancestor) → slugified directory name.
pub fn resolve(start: &Path, explicit: Option<&str>) -> Result<String> {
    if let Some(name) = explicit {
        return Ok(to_tag(name));
    }
    if let Some(tag) = read_marker(start)? {
        return Ok(tag);
    }
    let base = start
        .file_name()
        .and_then(|s| s.to_str())
        .context("cannot derive a project from the current directory; pass --project")?;
    Ok(to_tag(base))
}

/// Search `start` and its ancestors for a `.kiem` marker; return its project tag.
pub fn read_marker(start: &Path) -> Result<Option<String>> {
    for dir in start.ancestors() {
        let path = dir.join(MARKER);
        if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            return Ok(Some(parse_marker(&text)?));
        }
    }
    Ok(None)
}

/// Write (or overwrite) the `.kiem` marker in `dir`, binding it to `tag`.
pub fn write_marker(dir: &Path, tag: &str) -> Result<PathBuf> {
    let path = dir.join(MARKER);
    std::fs::write(&path, format!("project = \"{tag}\"\n"))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Add a one-line human pointer to `AGENTS.md` so an agent discovers the workflow
/// narratively. Idempotent (skips if a Kiem pointer is already present); creates
/// the file if absent. The machine-read binding remains the `.kiem` marker.
pub fn ensure_agents_pointer(dir: &Path, tag: &str) -> Result<()> {
    let path = dir.join("AGENTS.md");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.contains("Kiem project `") {
        return Ok(());
    }
    let line = format!(
        "\n<!-- kiem -->\nThis repo is Kiem project `{tag}`. Run `kiem todos` / `kiem notes` for project state, and record progress with `kiem note add` / `kiem todo check`.\n"
    );
    let body = if existing.is_empty() {
        format!("# Agent guide\n{line}")
    } else {
        format!("{existing}{line}")
    };
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Parse `project = "proj/<slug>"` from marker text (quotes and spacing tolerant).
fn parse_marker(text: &str) -> Result<String> {
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("project") {
            let value = rest
                .trim_start()
                .strip_prefix('=')
                .context("malformed .kiem: expected `project = \"proj/<slug>\"`")?;
            let value = value.trim().trim_matches('"').trim();
            if !value.is_empty() {
                return Ok(value.to_string());
            }
        }
    }
    bail!("malformed .kiem: no `project` key")
}

/// Build a `proj/<slug>` tag from a free-form name or an already-prefixed value.
pub fn to_tag(name: &str) -> String {
    let raw = name.strip_prefix(TAG_PREFIX).unwrap_or(name);
    format!("{TAG_PREFIX}{}", slugify(raw))
}

/// Slugify into characters a Kiem tag accepts (`[a-z0-9_/]` — the tag regex has
/// no `-`, so the separator is `_`): lowercase; spaces/dashes/underscores → `_`;
/// keep `[a-z0-9/]`; drop the rest; collapse repeats and trim leading/trailing `_`.
/// This guarantees `#proj/<slug>` round-trips through `extract_tags` intact.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_sep = false;
    for ch in name.chars() {
        match ch.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '/') => {
                out.push(c);
                prev_sep = false;
            }
            ' ' | '_' | '-' if !prev_sep && !out.is_empty() => {
                out.push('_');
                prev_sep = true;
            }
            _ => {}
        }
    }
    out.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_uses_tag_safe_chars() {
        // Separator is `_`, not `-` (the tag regex rejects `-`).
        assert_eq!(slugify("Kiem App"), "kiem_app");
        assert_eq!(slugify("  My__Cool  Project!! "), "my_cool_project");
        assert_eq!(slugify("work/meetings"), "work/meetings");
    }

    #[test]
    fn slug_survives_tag_extraction() {
        // The whole point of the `_` separator: `#proj/<slug>` must round-trip.
        let tag = to_tag("Kiem App");
        assert_eq!(tag, "proj/kiem_app");
        let derived = kiem_core::content::extract_tags(&format!("note body\n\n#{tag}"));
        assert_eq!(derived, vec![tag]);
    }

    #[test]
    fn to_tag_is_idempotent_on_prefixed_input() {
        assert_eq!(to_tag("Kiem App"), "proj/kiem_app");
        assert_eq!(to_tag("proj/kiem_app"), "proj/kiem_app");
    }

    #[test]
    fn marker_roundtrips_and_parses() {
        let dir = tempfile::tempdir().unwrap();
        write_marker(dir.path(), "proj/kiem_app").unwrap();
        assert_eq!(read_marker(dir.path()).unwrap().as_deref(), Some("proj/kiem_app"));
    }

    #[test]
    fn marker_found_in_ancestor_directory() {
        let dir = tempfile::tempdir().unwrap();
        write_marker(dir.path(), "proj/x").unwrap();
        let sub = dir.path().join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(read_marker(&sub).unwrap().as_deref(), Some("proj/x"));
    }

    #[test]
    fn resolve_precedence_explicit_marker_then_dirname() {
        let dir = tempfile::tempdir().unwrap();
        // No marker → slugified dir name.
        let from_name = resolve(dir.path(), None).unwrap();
        assert!(from_name.starts_with("proj/"));
        // Explicit override wins regardless of marker.
        assert_eq!(resolve(dir.path(), Some("Other Thing")).unwrap(), "proj/other_thing");
        // Marker wins over dir name.
        write_marker(dir.path(), "proj/marked").unwrap();
        assert_eq!(resolve(dir.path(), None).unwrap(), "proj/marked");
    }

    #[test]
    fn agents_pointer_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        ensure_agents_pointer(dir.path(), "proj/x").unwrap();
        let once = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        ensure_agents_pointer(dir.path(), "proj/x").unwrap();
        let twice = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert_eq!(once, twice, "pointer must not be appended twice");
        assert!(once.contains("proj/x"));
    }
}
