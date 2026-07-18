//! Project resolution and the committed `.kiem` marker.
//!
//! A project is the reserved tag `proj/<slug>`. A repo declares its project in a
//! small committed `.kiem` file (`project = "proj/<slug>"`) so the binding travels
//! with the repo across machines — Kiem itself stores no filesystem paths. The
//! agent's "current project" resolves from that marker (searched up the directory
//! tree), falling back to the slugified directory name when no marker exists.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

// The tag vocabulary (slugify/to_tag/ensure_tag) lives in kiem-core so the
// app reaches it through the FFI; this module keeps the CLI-only concepts —
// the committed repo marker and "current project" resolution.
pub use kiem_core::project::{ensure_tag, to_tag, TAG_PREFIX};

/// Marker filename committed at a project's repo root.
pub const MARKER: &str = ".kiem";

/// Resolve the current project tag: explicit override → `.kiem` marker (current
/// dir or any ancestor) → slugified directory name.
pub fn resolve(start: &Path, explicit: Option<&str>) -> Result<String> {
    if let Some(name) = explicit {
        return Ok(to_tag(name));
    }
    if let Some(tag) = read_marker(start)? {
        // Canonicalize through `to_tag` so a hand-edited marker (e.g.
        // `proj/My App`) resolves to the same tag `note add` will embed
        // (`proj/my_app`), instead of desyncing writes from queries.
        return Ok(to_tag(&tag));
    }
    let base = start
        .file_name()
        .and_then(|s| s.to_str())
        .context("cannot derive a project from the current directory; pass --project")?;
    Ok(to_tag(base))
}

/// Search `start` and its ancestors for a `.kiem` marker; return its project tag.
/// The walk stops at the repository root (a directory containing `.git`) so a
/// stray marker above the repo (e.g. `~/.kiem`) never captures an unrelated repo.
pub fn read_marker(start: &Path) -> Result<Option<String>> {
    for dir in start.ancestors() {
        let path = dir.join(MARKER);
        if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            return Ok(Some(parse_marker(&text)?));
        }
        if dir.join(".git").exists() {
            break; // repo root: don't escape into parent/home markers
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
/// Skips lines that merely start with `project` but are a different key
/// (`projects`, `project_owner`, …) and only errors once no `project` key is found.
fn parse_marker(text: &str) -> Result<String> {
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("project") else { continue };
        // The exact key `project`: the next non-space character must be `=`.
        let Some(value) = rest.trim_start().strip_prefix('=') else { continue };
        let value = value.trim().trim_matches('"').trim();
        if !value.is_empty() {
            return Ok(value.to_string());
        }
    }
    bail!("malformed .kiem: no `project` key")
}

/// `to_tag`, but a name with no slug-able characters is an error instead of
/// an empty string — for callers that must refuse rather than fall back.
pub fn require_tag(name: &str) -> Result<String> {
    let tag = to_tag(name);
    if tag.is_empty() {
        bail!("cannot derive a project name from {name:?}");
    }
    Ok(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ancestor_walk_stops_at_repo_root() {
        // A marker above the repo root must not capture the repo.
        let outer = tempfile::tempdir().unwrap();
        write_marker(outer.path(), "proj/outer").unwrap();
        let repo = outer.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let sub = repo.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        // No marker inside the repo → walk stops at .git, never sees proj/outer.
        assert_eq!(read_marker(&sub).unwrap(), None);
        // A marker at the repo root is still found.
        write_marker(&repo, "proj/inner").unwrap();
        assert_eq!(read_marker(&sub).unwrap().as_deref(), Some("proj/inner"));
    }

    #[test]
    fn parse_marker_skips_non_key_lines_and_errors_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        // A `project`-prefixed decoy key above the real key must not abort parsing.
        std::fs::write(dir.path().join(MARKER), "project_owner = \"me\"\nproject = \"proj/y\"\n").unwrap();
        assert_eq!(read_marker(dir.path()).unwrap().as_deref(), Some("proj/y"));

        std::fs::write(dir.path().join(MARKER), "name = \"x\"\n").unwrap();
        assert!(read_marker(dir.path()).is_err(), "no project key → error");
    }

    #[test]
    fn resolve_canonicalizes_a_hand_edited_marker() {
        let dir = tempfile::tempdir().unwrap();
        // A human wrote a non-canonical tag (capitals + space).
        std::fs::write(dir.path().join(MARKER), "project = \"proj/My App\"\n").unwrap();
        // resolve canonicalizes it to what `note add` will actually embed.
        assert_eq!(resolve(dir.path(), None).unwrap(), "proj/my_app");
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
