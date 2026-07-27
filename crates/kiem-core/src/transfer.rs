//! Note import/export as a directory of Markdown files. The layout is the
//! same in both directions — a folder is a project: either one flat folder of
//! `.md` files (= one project, named after the folder) or a folder of
//! subfolders (= one project per subfolder). One file per note, body
//! verbatim, so the inline `#proj/<slug>` tag and every checkbox todo
//! round-trip through the normal content derivation. The one non-verbatim
//! touch: a non-default note type travels as a `type:` line in the
//! frontmatter fence (core derives status from frontmatter, never type).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::note::{NoteMetadata, DEFAULT_NOTE_TYPE};
use crate::project;
use crate::store::{NoteStore, StoreError};

#[derive(Debug, Error)]
pub enum TransferError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(
        "cannot derive a project from folder {name:?} (for {file}); \
         rename the folder to letters/numbers, or choose a project explicitly"
    )]
    NoProject { name: String, file: String },
}

/// `io::Result` → `TransferError::Io` with a lazy human context.
fn io_ctx<T>(
    res: std::io::Result<T>,
    context: impl FnOnce() -> String,
) -> Result<T, TransferError> {
    res.map_err(|source| TransferError::Io {
        context: context(),
        source,
    })
}

/// Export every note that belongs to a project into `dir/<slug>/`. Notes
/// without a usable `proj/*` tag are skipped (export is project-folder-shaped;
/// a note with several project tags goes under its first one only). Returns
/// `(written, skipped)`.
pub fn export_all(store: &NoteStore, dir: &Path) -> Result<(usize, usize), TransferError> {
    export_all_with_progress(store, dir, &mut |_, _| {})
}

/// [`export_all`] reporting `(done, total)` after each note — for surfaces
/// that show a progress bar over a long transfer.
pub fn export_all_with_progress(
    store: &NoteStore,
    dir: &Path,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<(usize, usize), TransferError> {
    let mut written = 0;
    let mut skipped = 0;
    let notes = store.list_notes()?;
    let total = notes.len();
    for (done, meta) in notes.into_iter().enumerate() {
        // A nested slug (`proj/work/meetings`) becomes a nested folder path,
        // which `import` maps back to the same tag.
        let folder = meta
            .tags
            .iter()
            .find_map(|t| t.strip_prefix(project::TAG_PREFIX))
            .and_then(slug_folder);
        if let Some(folder) = folder {
            write_note(store, &meta, &dir.join(folder))?;
            written += 1;
        } else {
            skipped += 1;
        }
        progress(done + 1, total);
    }
    Ok((written, skipped))
}

/// Export one project flat into `dir` — the folder *is* the project.
pub fn export_project(store: &NoteStore, dir: &Path, tag: &str) -> Result<usize, TransferError> {
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

fn write_note(store: &NoteStore, meta: &NoteMetadata, folder: &Path) -> Result<(), TransferError> {
    let note = store
        .get_note(&meta.id)?
        .ok_or_else(|| StoreError::NotFound(meta.id.clone()))?;
    io_ctx(std::fs::create_dir_all(folder), || {
        format!("creating {}", folder.display())
    })?;
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
    io_ctx(std::fs::write(&path, body), || {
        format!("writing {}", path.display())
    })
}

/// How [`import`] assigns projects to the notes it creates.
#[derive(Clone, Copy)]
pub enum ProjectSource<'a> {
    /// A folder is a project: files in a subfolder join that subfolder's
    /// project; top-level files join a project named after the import dir
    /// itself (the flat-folder-is-a-project case).
    Folders,
    /// Everything joins this one project (a `proj/<slug>` tag).
    Tag(&'a str),
    /// No project at all — notes keep only the tags already in their bodies
    /// (e.g. importing an exported Bear/Obsidian dump that isn't one project).
    None,
}

/// Import every `.md` file under `dir` as a note; returns the created
/// `(file, metadata)` pairs plus how many files were skipped as duplicates.
/// The project tag (per `source`) is appended to the body unless already
/// present, and a file whose body already exists is skipped, so re-importing
/// the same directory is a no-op.
pub fn import(
    store: &mut NoteStore,
    dir: &Path,
    author: &str,
    source: ProjectSource,
) -> Result<(Vec<(PathBuf, NoteMetadata)>, usize), TransferError> {
    import_with_progress(store, dir, author, source, &mut |_, _| {})
}

/// [`import`] reporting `(done, total)` after each file — for surfaces that
/// show a progress bar over a long transfer.
pub fn import_with_progress(
    store: &mut NoteStore,
    dir: &Path,
    author: &str,
    source: ProjectSource,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<(Vec<(PathBuf, NoteMetadata)>, usize), TransferError> {
    // Canonicalize so importing `.` (file_name() == None) and trailing `..`
    // resolve to the folder's real name — and so a bad path fails here, not
    // per-file after a partial import.
    let dir = io_ctx(dir.canonicalize(), || {
        format!("resolving directory {}", dir.display())
    })?;
    let mut files = Vec::new();
    collect_md_files(&dir, &mut files)?;
    files.sort();
    // Resolve every file's project up front, so one bad folder name fails the
    // whole import before anything is created (all-or-nothing) — not mid-loop
    // after earlier folders' notes were already written.
    let tagged: Vec<(PathBuf, Option<String>)> = files
        .into_iter()
        .map(|file| {
            let tag = match source {
                ProjectSource::Tag(tag) => Some(tag.to_string()),
                ProjectSource::Folders => Some(tag_for(&dir, &file)?),
                ProjectSource::None => None,
            };
            Ok((file, tag))
        })
        .collect::<Result<_, TransferError>>()?;

    let mut created = Vec::new();
    let mut skipped_duplicates = 0;
    // Duplicate check: existing bodies loaded once per project (hydrating
    // every note per FILE made a 400-file import take minutes), and each
    // created body joins the set so identical files within one import batch
    // still dedupe.
    let mut existing: HashMap<String, HashSet<String>> = HashMap::new();
    let total = tagged.len();
    // `bulk`: one SQLite transaction + one search-index rebuild instead of
    // per-note commits — and a mid-import error rolls everything back, so
    // the all-or-nothing promise covers I/O failures too.
    store.bulk(|store| -> Result<(), TransferError> {
        for (done, (file, tag)) in tagged.into_iter().enumerate() {
            let text = io_ctx(std::fs::read_to_string(&file), || {
                format!("reading {}", file.display())
            })?;
            if text.trim().is_empty() {
                progress(done + 1, total);
                continue;
            }
            let body = match &tag {
                Some(tag) => project::ensure_tag(&text, tag),
                None => text,
            };
            // Untagged imports ("" key) compare against every note — with no
            // project to scope to, a duplicate is any note with the same body.
            let key = tag.clone().unwrap_or_default();
            if !existing.contains_key(&key) {
                let known = match &tag {
                    Some(tag) => store.list_by_tag(tag)?,
                    None => store.list_notes()?,
                };
                let mut set = HashSet::new();
                for m in known {
                    let note = store
                        .get_note(&m.id)?
                        .ok_or_else(|| StoreError::NotFound(m.id.clone()))?;
                    set.insert(note.body.as_str().to_string());
                }
                existing.insert(key.clone(), set);
            }
            let bodies = existing.get_mut(&key).expect("just inserted");
            if bodies.contains(&body) {
                skipped_duplicates += 1;
            } else {
                let note_type = frontmatter_type(&body).unwrap_or_default();
                let meta = store.create_note_with_type(&body, author, note_type)?;
                bodies.insert(body);
                created.push((file, meta));
            }
            progress(done + 1, total);
        }
        Ok(())
    })?;
    Ok((created, skipped_duplicates))
}

/// The project tag for a file: its parent folder path relative to the import
/// root, slugified — or the root folder's own name for top-level files. A
/// folder that slugifies to nothing (or to a slug with an empty `//`
/// component, which would desync export paths from tags) is an error.
fn tag_for(dir: &Path, file: &Path) -> Result<String, TransferError> {
    let parent = file.parent().unwrap_or(dir);
    let name = if parent == dir {
        dir.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string()
    } else {
        let rel = parent.strip_prefix(dir).unwrap_or(parent);
        rel.to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/")
    };
    let tag = project::to_tag(&name);
    let slug = tag.strip_prefix(project::TAG_PREFIX).unwrap_or("");
    if slug.is_empty() || slug.split('/').any(|c| c.is_empty()) {
        return Err(TransferError::NoProject {
            name,
            file: file.display().to_string(),
        });
    }
    Ok(tag)
}

/// Recursively gather `.md` files, skipping dot-directories (`.git`,
/// `.obsidian`, …). Uses the entry's own file type (lstat semantics), so a
/// symlinked directory is never followed — a cycle like `ln -s . loop` must
/// not recurse forever, and a symlink into a sibling tree must not import it.
fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), TransferError> {
    let entries = io_ctx(std::fs::read_dir(dir), || {
        format!("reading directory {}", dir.display())
    })?;
    for entry in entries {
        let entry = io_ctx(entry, || format!("reading directory {}", dir.display()))?;
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        let file_type = io_ctx(entry.file_type(), || {
            format!("reading file type of {}", path.display())
        })?;
        if file_type.is_dir() {
            collect_md_files(&path, out)?;
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("md"))
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
        Some(i)
            if lines[1..=i]
                .iter()
                .any(|l| l.trim_start().starts_with("type:")) =>
        {
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
/// [`crate::content::parse_frontmatter_status`] (which deliberately reads
/// only `status:`).
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

    /// The only test left in this module:  and
    ///  are private formatting details with no reason to be
    /// public, so their round-trip is checked from in here. Everything that
    /// drives the public export/import API moved to
    /// .
    #[test]
    fn frontmatter_type_round_trips_through_existing_fences() {
        // No fence → one is prepended; the parser reads it back.
        let typed = with_frontmatter_type("# A\nbody", "plan");
        assert_eq!(typed, "---\ntype: plan\n---\n\n# A\nbody");
        assert_eq!(frontmatter_type(&typed), Some("plan"));
        // Existing fence (e.g. `status:`) gains a type line, once.
        let merged = with_frontmatter_type("---\nstatus: active\n---\n# A", "plan");
        assert_eq!(merged, "---\ntype: plan\nstatus: active\n---\n# A");
        assert_eq!(
            with_frontmatter_type(&merged, "review"),
            merged,
            "explicit type wins"
        );
        // No fence, no type.
        assert_eq!(frontmatter_type("# A\nbody"), None);
    }
}
