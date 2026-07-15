//! `kiem export` / `kiem import`: exchange notes as a directory of Markdown
//! files. The layout is the same in both directions — a folder is a project:
//! either one flat folder of `.md` files (= one project, named after the
//! folder) or a folder of subfolders (= one project per subfolder). One file
//! per note, body verbatim, so the inline `#proj/<slug>` tag and every
//! checkbox todo round-trip through the normal content derivation. The one
//! non-verbatim touch: a non-default note type travels as a `type:` line in
//! the frontmatter fence (CLI-local; core derives status from frontmatter,
//! never type).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use kiem_core::note::{NoteMetadata, DEFAULT_NOTE_TYPE};
use kiem_core::store::NoteStore;

use crate::project;

/// Export every note that belongs to a project into `dir/<slug>/`. Notes
/// without a usable `proj/*` tag are skipped (export is project-folder-shaped;
/// a note with several project tags goes under its first one only). Returns
/// `(written, skipped)`.
pub fn export_all(store: &NoteStore, dir: &Path) -> Result<(usize, usize)> {
    let mut written = 0;
    let mut skipped = 0;
    for meta in store.list_notes()? {
        // A nested slug (`proj/work/meetings`) becomes a nested folder path,
        // which `import` maps back to the same tag.
        let folder = meta
            .tags
            .iter()
            .find_map(|t| t.strip_prefix(project::TAG_PREFIX))
            .and_then(slug_folder);
        let Some(folder) = folder else {
            skipped += 1;
            continue;
        };
        write_note(store, &meta, &dir.join(folder))?;
        written += 1;
    }
    Ok((written, skipped))
}

/// Export one project flat into `dir` — the folder *is* the project.
pub fn export_project(store: &NoteStore, dir: &Path, tag: &str) -> Result<usize> {
    let notes = store.list_by_tag(tag)?;
    for meta in &notes {
        write_note(store, meta, dir)?;
    }
    Ok(notes.len())
}

/// Relative folder path for a project slug — `None` when nothing remains.
/// Empty components are dropped, never joined: the tag regex admits a
/// degenerate `proj//sub`, whose raw suffix `/sub` is absolute and would make
/// `Path::join` *replace* the export dir instead of nesting under it.
fn slug_folder(slug: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = slug.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.iter().collect())
    }
}

fn write_note(store: &NoteStore, meta: &NoteMetadata, folder: &Path) -> Result<()> {
    let note = store
        .get_note(&meta.id)?
        .with_context(|| format!("note not found: {}", meta.id))?;
    std::fs::create_dir_all(folder)
        .with_context(|| format!("creating {}", folder.display()))?;
    // Stable, unique filename: title slug + short id, so re-exporting an
    // unchanged note overwrites its file. (A renamed or trashed note leaves
    // its old file behind — re-export into a fresh dir when that matters.)
    let mut stem = project::slugify(&meta.title).replace('/', "_");
    if stem.is_empty() {
        stem = "untitled".to_string();
    }
    let short_id: String = meta.id.chars().take(8).collect();
    let path = folder.join(format!("{stem}-{short_id}.md"));
    let body = if meta.note_type == DEFAULT_NOTE_TYPE {
        note.body.as_str().to_string()
    } else {
        with_frontmatter_type(note.body.as_str(), &meta.note_type)
    };
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))
}

/// Import every `.md` file under `dir` as a note; returns the created
/// `(file, metadata)` pairs plus how many files were skipped as duplicates.
/// A file's project is its parent folder, slugified: files in a subfolder get
/// that subfolder's project; files at the top level get a project named after
/// `dir` itself (the flat-folder-is-a-project case). `override_tag` forces
/// one project for everything. The tag is appended to the body unless already
/// present, and a file whose body already exists in the target project is
/// skipped, so re-importing the same directory is a no-op.
pub fn import(
    store: &mut NoteStore,
    dir: &Path,
    author: &str,
    override_tag: Option<&str>,
) -> Result<(Vec<(PathBuf, NoteMetadata)>, usize)> {
    // Canonicalize so `kiem import .` (file_name() == None) and trailing
    // `..` resolve to the folder's real name — and so a bad path fails here,
    // not per-file after a partial import.
    let dir = dir
        .canonicalize()
        .with_context(|| format!("resolving directory {}", dir.display()))?;
    let mut files = Vec::new();
    collect_md_files(&dir, &mut files)?;
    files.sort();

    let mut created = Vec::new();
    let mut skipped_duplicates = 0;
    for file in files {
        let tag = match override_tag {
            Some(tag) => tag.to_string(),
            None => tag_for(&dir, &file)?,
        };
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        if text.trim().is_empty() {
            continue;
        }
        let body = project::ensure_tag(&text, &tag);
        // ponytail: O(files×notes) body rescan; cache bodies per tag if imports get big
        let mut exists = false;
        for m in store.list_by_tag(&tag)? {
            let note = store
                .get_note(&m.id)?
                .with_context(|| format!("note not found: {}", m.id))?;
            if note.body.as_str() == body {
                exists = true;
                break;
            }
        }
        if exists {
            skipped_duplicates += 1;
            continue;
        }
        let note_type = frontmatter_type(&body).unwrap_or_default();
        let meta = store.create_note_with_type(&body, author, note_type)?;
        created.push((file, meta));
    }
    Ok((created, skipped_duplicates))
}

/// The project tag for a file: its parent folder path relative to the import
/// root, slugified — or the root folder's own name for top-level files. A
/// folder that slugifies to nothing (or to a slug with an empty `//`
/// component, which would desync export paths from tags) is an error.
fn tag_for(dir: &Path, file: &Path) -> Result<String> {
    let parent = file.parent().unwrap_or(dir);
    let name = if parent == dir {
        dir.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_string()
    } else {
        let rel = parent.strip_prefix(dir).unwrap_or(parent);
        rel.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/")
    };
    let tag = project::to_tag(&name);
    let slug = tag.strip_prefix(project::TAG_PREFIX).unwrap_or("");
    if slug.is_empty() || slug.split('/').any(|c| c.is_empty()) {
        bail!(
            "cannot derive a project from {:?} (folder of {}); pass --project",
            name,
            file.display()
        );
    }
    Ok(tag)
}

/// Recursively gather `.md` files, skipping dot-directories (`.git`,
/// `.obsidian`, …). Uses the entry's own file type (lstat semantics), so a
/// symlinked directory is never followed — a cycle like `ln -s . loop` must
/// not recurse forever, and a symlink into a sibling tree must not import it.
fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading directory {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading directory {}", dir.display()))?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        let file_type = entry
            .file_type()
            .with_context(|| format!("reading file type of {}", path.display()))?;
        if file_type.is_dir() {
            collect_md_files(&path, out)?;
        } else if file_type.is_file()
            && path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Body with `type: <t>` present in a leading frontmatter fence — added to an
/// existing fence (unless one already declares a type), or a new fence
/// prepended. `frontmatter_type` is the exact inverse.
fn with_frontmatter_type(body: &str, note_type: &str) -> String {
    let lines: Vec<&str> = body.split('\n').collect();
    let close = if lines.first().map(|l| l.trim_end()) == Some("---") {
        lines.iter().skip(1).position(|l| l.trim_end() == "---")
    } else {
        None
    };
    match close {
        None => format!("---\ntype: {note_type}\n---\n\n{body}"),
        Some(i) if lines[1..=i].iter().any(|l| l.trim_start().starts_with("type:")) => {
            body.to_string()
        }
        Some(_) => {
            let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
            out.insert(1, format!("type: {note_type}"));
            out.join("\n")
        }
    }
}

/// `type: <t>` from a leading frontmatter fence, if any. Mirrors the shape of
/// core's `parse_frontmatter_status` (which deliberately reads only `status:`).
fn frontmatter_type(body: &str) -> Option<&str> {
    let mut lines = body.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    for line in lines {
        if line.trim_end() == "---" {
            return None;
        }
        if let Some(value) = line.strip_prefix("type:") {
            return Some(value.trim());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_store() -> (tempfile::TempDir, NoteStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = NoteStore::open_dir(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn export_writes_project_folders_and_skips_unfiled_notes() {
        let (_guard, mut store) = new_store();
        store.create_note("# Plan\n\n- [ ] ship it\n\n#proj/demo", "t").unwrap();
        store.create_note("# Nested\n\n#proj/work/meetings", "t").unwrap();
        store.create_note("# No project here", "t").unwrap();
        // A slash in the title must stay in the filename stem, not become a
        // subfolder that import would read as a different project.
        store.create_note("# work/meetings agenda\n\n#proj/demo", "t").unwrap();
        // Degenerate tags: `proj//sub` must not escape the export dir
        // (its raw suffix `/sub` is absolute); bare `proj/` has no folder.
        store.create_note("# Escapee\n\n#proj//sub", "t").unwrap();

        let out = tempfile::tempdir().unwrap();
        let (written, skipped) = export_all(&store, out.path()).unwrap();
        assert_eq!((written, skipped), (4, 1));

        let mut demo: Vec<String> = std::fs::read_dir(out.path().join("demo"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        demo.sort();
        assert!(demo[0].starts_with("plan-"));
        assert!(demo[1].starts_with("work_meetings_agenda-"));
        let body =
            std::fs::read_to_string(out.path().join("demo").join(&demo[0])).unwrap();
        assert_eq!(body, "# Plan\n\n- [ ] ship it\n\n#proj/demo");
        // Nested slug → nested folder path; `proj//sub` lands inside the dir.
        assert_eq!(std::fs::read_dir(out.path().join("work/meetings")).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(out.path().join("sub")).unwrap().count(), 1);
        assert!(!Path::new("/sub").exists());
    }

    #[test]
    fn import_maps_folders_to_projects_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("inbox");
        std::fs::create_dir_all(root.join("proj-a")).unwrap();
        // Subfolder file: project from the subfolder. No inline tag → appended.
        std::fs::write(root.join("proj-a/one.md"), "# One\n\n- [ ] todo one\n").unwrap();
        // Top-level file: the flat-folder case — project from the root folder name.
        std::fs::write(root.join("two.md"), "# Two").unwrap();
        // Non-markdown and dot-dirs are ignored.
        std::fs::write(root.join("notes.txt"), "not me").unwrap();
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        std::fs::write(root.join(".obsidian/three.md"), "hidden").unwrap();
        // A symlink cycle must not recurse (and must not abort the import).
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();

        let (_guard, mut store) = new_store();
        // `inbox/proj-a/..` resolves to the root: the `kiem import .` shape.
        let (created, skipped) =
            import(&mut store, &root.join("proj-a/.."), "t", None).unwrap();
        assert_eq!(created.len(), 2);
        assert_eq!(skipped, 0);

        let a = store.list_by_tag("proj/proj_a").unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].title, "One");
        assert_eq!(store.list_todo_items_for_tag("proj/proj_a").unwrap().len(), 1);
        assert_eq!(store.list_by_tag("proj/inbox").unwrap()[0].title, "Two");

        // Re-import: everything already exists, nothing is duplicated.
        let (again, skipped) = import(&mut store, &root, "t", None).unwrap();
        assert!(again.is_empty());
        assert_eq!(skipped, 2);
        assert_eq!(store.list_notes().unwrap().len(), 2);
    }

    #[test]
    fn export_import_round_trip_preserves_bodies_projects_and_types() {
        let (_guard, mut store) = new_store();
        store.create_note("# A\n\n- [ ] alpha\n\n#proj/demo", "t").unwrap();
        store.create_note_with_type("# B\n\nbody\n\n#proj/other", "t", "plan").unwrap();

        let out = tempfile::tempdir().unwrap();
        export_all(&store, out.path()).unwrap();

        let (_guard2, mut fresh) = new_store();
        let (created, _) = import(&mut fresh, out.path(), "t", None).unwrap();
        assert_eq!(created.len(), 2);
        let a = &fresh.list_by_tag("proj/demo").unwrap()[0];
        assert_eq!(
            fresh.get_note(&a.id).unwrap().unwrap().body.as_str(),
            "# A\n\n- [ ] alpha\n\n#proj/demo"
        );
        assert_eq!(fresh.list_todo_items_for_tag("proj/demo").unwrap()[0].text, "alpha");
        // The non-default type traveled via the frontmatter fence.
        let b = &fresh.list_by_tag("proj/other").unwrap()[0];
        assert_eq!(b.title, "B");
        assert_eq!(b.note_type, "plan");
        // And re-importing the typed note is still a no-op.
        let (created, skipped) = import(&mut fresh, out.path(), "t", None).unwrap();
        assert!(created.is_empty());
        assert_eq!(skipped, 2);
    }

    #[test]
    fn export_project_writes_flat_and_import_honors_override() {
        let (_guard, mut store) = new_store();
        store.create_note("# Solo\n\n#proj/demo", "t").unwrap();

        let out = tempfile::tempdir().unwrap();
        assert_eq!(export_project(&store, out.path(), "proj/demo").unwrap(), 1);
        // Flat: the file sits directly in the folder, no subfolder.
        let entry = std::fs::read_dir(out.path()).unwrap().next().unwrap().unwrap();
        assert!(entry.path().is_file());

        let (_guard2, mut fresh) = new_store();
        import(&mut fresh, out.path(), "t", Some("proj/forced")).unwrap();
        let meta = &fresh.list_by_tag("proj/forced").unwrap()[0];
        // Original inline tag survives; the override is appended alongside.
        assert!(fresh.list_by_tag("proj/demo").unwrap().iter().any(|m| m.id == meta.id));
    }

    #[test]
    fn import_rejects_folders_that_slugify_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("!!!");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.md"), "# A").unwrap();

        let (_guard, mut store) = new_store();
        let err = import(&mut store, &root, "t", None).unwrap_err();
        assert!(err.to_string().contains("--project"), "unexpected error: {err:#}");
        assert!(store.list_notes().unwrap().is_empty(), "nothing may be written");
        // The override rescues the same directory.
        let (created, _) = import(&mut store, &root, "t", Some("proj/x")).unwrap();
        assert_eq!(created.len(), 1);
    }

    #[test]
    fn frontmatter_type_round_trips_through_existing_fences() {
        // No fence → one is prepended; the parser reads it back.
        let typed = with_frontmatter_type("# A\nbody", "plan");
        assert_eq!(typed, "---\ntype: plan\n---\n\n# A\nbody");
        assert_eq!(frontmatter_type(&typed), Some("plan"));
        // Existing fence (e.g. `status:`) gains a type line, once.
        let merged = with_frontmatter_type("---\nstatus: active\n---\n# A", "plan");
        assert_eq!(merged, "---\ntype: plan\nstatus: active\n---\n# A");
        assert_eq!(with_frontmatter_type(&merged, "review"), merged, "explicit type wins");
        // No fence, no type.
        assert_eq!(frontmatter_type("# A\nbody"), None);
    }
}
