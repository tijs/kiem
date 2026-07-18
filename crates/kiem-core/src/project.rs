//! The `proj/<slug>` project-tag vocabulary: slugify a free-form name into a
//! tag and stamp a body with its project. Shared by the CLI and (through the
//! FFI) the app; the Swift mirror `KiemModel.projectTag(for:)` is held to the
//! same rules by `fixtures/project-slug.json`.

/// Tag namespace that makes a tag a project.
pub const TAG_PREFIX: &str = "proj/";

/// Build a `proj/<slug>` tag from a free-form name or an already-prefixed value.
/// Returns an empty string when the name has no slug-able characters, so callers
/// can reject it rather than creating a degenerate `proj/` tag.
pub fn to_tag(name: &str) -> String {
    let raw = name.strip_prefix(TAG_PREFIX).unwrap_or(name);
    let slug = slugify(raw);
    if slug.is_empty() {
        String::new()
    } else {
        format!("{TAG_PREFIX}{slug}")
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

/// Body with `#tag` appended — unless the body's derived tags already include
/// it (a hand-tagged note must not carry it twice). The single definition of
/// the rule `kiem note add` and note import share.
pub fn ensure_tag(body: &str, tag: &str) -> String {
    if crate::content::extract_tags(body).iter().any(|t| t == tag) {
        body.to_string()
    } else {
        format!("{}\n\n#{tag}", body.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
        let derived = crate::content::extract_tags(&format!("note body\n\n#{tag}"));
        assert_eq!(derived, vec![tag]);
    }

    #[test]
    fn to_tag_is_idempotent_on_prefixed_input() {
        assert_eq!(to_tag("Kiem App"), "proj/kiem_app");
        assert_eq!(to_tag("proj/kiem_app"), "proj/kiem_app");
    }

    #[test]
    fn empty_name_yields_empty_tag() {
        assert_eq!(to_tag("!!!"), "");
        assert_eq!(to_tag("   "), "");
    }

    #[test]
    fn ensure_tag_appends_once() {
        assert_eq!(ensure_tag("body\n", "proj/x"), "body\n\n#proj/x");
        let tagged = "body\n\n#proj/x";
        assert_eq!(ensure_tag(tagged, "proj/x"), tagged);
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
}
