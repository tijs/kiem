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

/// Build a `proj/<slug>` tag from a free-form name or an already-prefixed value.
/// Returns an empty string when the name has no slug-able characters, so callers
/// can reject it (matching the app's empty-slug guard) rather than creating a
/// degenerate `proj/` tag.
pub fn to_tag(name: &str) -> String {
    let raw = name.strip_prefix(TAG_PREFIX).unwrap_or(name);
    let slug = slugify(raw);
    if slug.is_empty() {
        String::new()
    } else {
        format!("{TAG_PREFIX}{slug}")
    }
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

/// Body with `#tag` appended — unless the body's derived tags already include
/// it (a hand-tagged note must not carry it twice). The single definition of
/// the rule `kiem note add` and `kiem import` share.
pub fn ensure_tag(body: &str, tag: &str) -> String {
    if kiem_core::content::extract_tags(body).iter().any(|t| t == tag) {
        body.to_string()
    } else {
        format!("{}\n\n#{tag}", body.trim_end())
    }
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
    fn empty_name_yields_empty_tag() {
        assert_eq!(to_tag("!!!"), "");
        assert_eq!(to_tag("   "), "");
    }

    #[test]
    fn slug_parity_fixture_matches_rust_impl() {
        // Cross-language contract: every case here must also pass against the
        // Swift `projectTag(for:)` mirror (see fixtures/project-slug.json).
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/project-slug.json");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        for case in json["cases"].as_array().unwrap() {
            let input = case["input"].as_str().unwrap();
            let expected = case["tag"].as_str().unwrap();
            assert_eq!(to_tag(input), expected, "slug mismatch for {input:?}");
        }
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
