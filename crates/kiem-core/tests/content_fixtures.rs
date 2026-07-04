//! Validates `kiem-core`'s content derivation against the shared, language-neutral
//! fixture contract (`fixtures/content-derivation.json`). Pulp's Swift
//! `ContentAnalyzer` runs the same contract against its own vendored copy in the
//! Pulp repo. Each project tests itself against the contract; keeping the two
//! copies in sync is a release/CI concern, not a cross-repo filesystem test from
//! here.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContentFixture {
    name: String,
    input: String,
    title: String,
    tags: Vec<String>,
    has_unchecked_todos: bool,
    #[serde(default)]
    status: Option<String>,
}

fn workspace_root() -> PathBuf {
    // crates/kiem-core -> workspace root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root resolves")
}

fn canonical_fixture_path() -> PathBuf {
    workspace_root().join("fixtures/content-derivation.json")
}

fn load_fixtures(path: &Path) -> Vec<ContentFixture> {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read fixtures at {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("fixtures parse as JSON")
}

#[test]
fn derivation_matches_shared_contract() {
    let fixtures = load_fixtures(&canonical_fixture_path());
    assert!(!fixtures.is_empty(), "fixture set must not be empty");

    for f in &fixtures {
        let (status, rest) = kiem_core::content::parse_frontmatter_status(&f.input);
        assert_eq!(status, f.status, "status mismatch for fixture `{}`", f.name);
        assert_eq!(
            kiem_core::content::derive_title(rest),
            f.title,
            "title mismatch for fixture `{}`",
            f.name
        );
        assert_eq!(
            kiem_core::content::extract_tags(rest),
            f.tags,
            "tags mismatch for fixture `{}`",
            f.name
        );
        assert_eq!(
            kiem_core::content::has_unchecked_todos(&f.input),
            f.has_unchecked_todos,
            "unchecked-todo mismatch for fixture `{}`",
            f.name
        );
    }
}
