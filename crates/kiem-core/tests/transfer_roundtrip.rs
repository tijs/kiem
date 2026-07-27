//! Export and import as a directory of Markdown files, driven end to end
//! through the public API — a real store, a real temp directory, real files.
//!
//! Round-tripping is the point of most of these: what comes back out has to
//! be what went in, including the project a note belongs to and its type.

use std::path::Path;

use kiem_core::store::NoteStore;
use kiem_core::transfer::*;

fn new_store() -> (tempfile::TempDir, NoteStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::open_dir(dir.path()).unwrap();
    (dir, store)
}

#[test]
fn export_writes_project_folders_and_skips_unfiled_notes() {
    let (_guard, mut store) = new_store();
    store
        .create_note("# Plan\n\n- [ ] ship it\n\n#proj/demo", "t")
        .unwrap();
    store
        .create_note("# Nested\n\n#proj/work/meetings", "t")
        .unwrap();
    store.create_note("# No project here", "t").unwrap();
    // A slash in the title must stay in the filename stem, not become a
    // subfolder that import would read as a different project.
    store
        .create_note("# work/meetings agenda\n\n#proj/demo", "t")
        .unwrap();
    // Degenerate tags: `proj//sub` must not escape the export dir
    // (its raw suffix `/sub` is absolute); bare `proj/` has no folder.
    store.create_note("# Escapee\n\n#proj//sub", "t").unwrap();

    let out = tempfile::tempdir().unwrap();
    let mut seen = Vec::new();
    let (written, skipped) = export_all_with_progress(&store, out.path(), &mut |d, t| {
        seen.push((d, t));
    })
    .unwrap();
    assert_eq!((written, skipped), (4, 1));
    // Progress counts every listed note (skipped ones included), so a
    // bar driven by it always reaches 100%.
    assert_eq!(seen, vec![(1, 5), (2, 5), (3, 5), (4, 5), (5, 5)]);

    let mut demo: Vec<String> = std::fs::read_dir(out.path().join("demo"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    demo.sort();
    assert!(demo[0].starts_with("plan-"));
    assert!(demo[1].starts_with("work_meetings_agenda-"));
    let body = std::fs::read_to_string(out.path().join("demo").join(&demo[0])).unwrap();
    assert_eq!(body, "# Plan\n\n- [ ] ship it\n\n#proj/demo");
    // Nested slug → nested folder path; `proj//sub` lands inside the dir.
    assert_eq!(
        std::fs::read_dir(out.path().join("work/meetings"))
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_dir(out.path().join("sub")).unwrap().count(),
        1
    );
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
    let (created, skipped) = import(
        &mut store,
        &root.join("proj-a/.."),
        "t",
        ProjectSource::Folders,
    )
    .unwrap();
    assert_eq!(created.len(), 2);
    assert_eq!(skipped, 0);

    let a = store.list_by_tag("proj/proj_a").unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].title, "One");
    assert_eq!(
        store.list_todo_items_for_tag("proj/proj_a").unwrap().len(),
        1
    );
    assert_eq!(store.list_by_tag("proj/inbox").unwrap()[0].title, "Two");

    // Re-import: everything already exists, nothing is duplicated.
    let (again, skipped) = import(&mut store, &root, "t", ProjectSource::Folders).unwrap();
    assert!(again.is_empty());
    assert_eq!(skipped, 2);
    assert_eq!(store.list_notes().unwrap().len(), 2);
}

#[test]
fn export_import_round_trip_preserves_bodies_projects_and_types() {
    let (_guard, mut store) = new_store();
    store
        .create_note("# A\n\n- [ ] alpha\n\n#proj/demo", "t")
        .unwrap();
    store
        .create_note_with_type("# B\n\nbody\n\n#proj/other", "t", "plan")
        .unwrap();

    let out = tempfile::tempdir().unwrap();
    export_all(&store, out.path()).unwrap();

    let (_guard2, mut fresh) = new_store();
    let (created, _) = import(&mut fresh, out.path(), "t", ProjectSource::Folders).unwrap();
    assert_eq!(created.len(), 2);
    let a = &fresh.list_by_tag("proj/demo").unwrap()[0];
    assert_eq!(
        fresh.get_note(&a.id).unwrap().unwrap().body.as_str(),
        "# A\n\n- [ ] alpha\n\n#proj/demo"
    );
    assert_eq!(
        fresh.list_todo_items_for_tag("proj/demo").unwrap()[0].text,
        "alpha"
    );
    // The non-default type traveled via the frontmatter fence.
    let b = &fresh.list_by_tag("proj/other").unwrap()[0];
    assert_eq!(b.title, "B");
    assert_eq!(b.note_type, "plan");
    // And re-importing the typed note is still a no-op.
    let (created, skipped) =
        import(&mut fresh, out.path(), "t", ProjectSource::Folders).unwrap();
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
    let entry = std::fs::read_dir(out.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert!(entry.path().is_file());

    let (_guard2, mut fresh) = new_store();
    import(
        &mut fresh,
        out.path(),
        "t",
        ProjectSource::Tag("proj/forced"),
    )
    .unwrap();
    let meta = &fresh.list_by_tag("proj/forced").unwrap()[0];
    // Original inline tag survives; the override is appended alongside.
    assert!(fresh
        .list_by_tag("proj/demo")
        .unwrap()
        .iter()
        .any(|m| m.id == meta.id));
}

#[test]
fn import_rejects_folders_that_slugify_to_nothing_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("!!!");
    // A valid subfolder that sorts BEFORE the failing top-level file:
    // all-or-nothing means even it must not be imported when any folder
    // in the batch has no derivable project.
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::write(root.join("a/ok.md"), "# OK").unwrap();
    std::fs::write(root.join("z.md"), "# Z").unwrap();

    let (_guard, mut store) = new_store();
    let err = import(&mut store, &root, "t", ProjectSource::Folders).unwrap_err();
    assert!(
        err.to_string().contains("choose a project"),
        "unexpected error: {err}"
    );
    assert!(
        store.list_notes().unwrap().is_empty(),
        "nothing may be written"
    );
    // An explicit project rescues the same directory.
    let (created, _) = import(&mut store, &root, "t", ProjectSource::Tag("proj/x")).unwrap();
    assert_eq!(created.len(), 2);
}

#[test]
fn import_without_projects_keeps_notes_untagged_and_dedupes_store_wide() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("bear-dump");
    std::fs::create_dir_all(root.join("archive")).unwrap();
    // A flat dump: no project should be minted from any folder name.
    std::fs::write(root.join("groceries.md"), "# Groceries\n\nmilk #errands").unwrap();
    std::fs::write(root.join("archive/old.md"), "# Old").unwrap();
    // Identical body twice IN the batch: the second file must dedupe
    // against the first one's just-created note, not slip through.
    std::fs::write(root.join("groceries-copy.md"), "# Groceries\n\nmilk #errands").unwrap();

    let (_guard, mut store) = new_store();
    // An identical note already in the store, in NO project — the
    // duplicate check must still find it without a tag to scope to.
    store.create_note("# Old", "t").unwrap();

    let (created, skipped) = import(&mut store, &root, "t", ProjectSource::None).unwrap();
    assert_eq!(created.len(), 1);
    assert_eq!(skipped, 2, "store-wide dupe + intra-batch dupe");
    let groceries = &created[0].1;
    assert_eq!(groceries.title, "Groceries");
    // Bulk import defers indexing to one rebuild — search must still see
    // the imported note afterwards.
    assert_eq!(
        store.search("groceries", 10).unwrap()[0].note_id,
        groceries.id
    );
    // Only the body's own tag — no proj/* was added anywhere.
    assert_eq!(groceries.tags, vec!["errands"]);
    assert!(store
        .list_tags()
        .unwrap()
        .iter()
        .all(|(t, _)| !t.starts_with("proj/")));
    // Progress reported per file with a stable total.
    let mut seen = Vec::new();
    let (_, _) =
        import_with_progress(&mut store, &root, "t", ProjectSource::None, &mut |d, t| {
            seen.push((d, t));
        })
        .unwrap();
    assert_eq!(seen, vec![(1, 3), (2, 3), (3, 3)]);
}
