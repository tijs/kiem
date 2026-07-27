//! The note commands: create, read, edit and list. The bulk of `kiem`'s
//! surface, and the part agents drive most, so every one of these takes the
//! `--json` flag through as `as_json` and shapes both outputs in one place.

use std::path::Path;

use anyhow::{Context, Result};
use kiem_core::store::NoteStore;
use serde_json::json;

use crate::args::NoteAction;
use crate::author;
use crate::project;
use crate::output::{
    compose_body, display_title, not_found_context, print_json, read_stdin, tag_suffix,
};

pub fn create(store: &mut NoteStore, data_dir: &Path, title: Option<String>, body: Option<String>, as_json: bool) -> Result<()> {
    let body = compose_body(title, body)?;
    let meta = store.create_note(&body, &author(data_dir)?)?;
    if as_json {
        print_json(&serde_json::to_value(&meta)?)?;
    } else {
        println!("Created: {} ({})", display_title(&meta), meta.id);
    }
    Ok(())
}

pub fn list(store: &NoteStore, tag: Option<String>, as_json: bool) -> Result<()> {
    let notes = match tag {
        Some(tag) => store.list_by_tag(&tag)?,
        None => store.list_notes()?,
    };
    if as_json {
        print_json(&serde_json::to_value(&notes)?)?;
    } else {
        for m in &notes {
            println!(
                "{}  {}  {}{}",
                m.id,
                m.modified_at,
                display_title(m),
                tag_suffix(m)
            );
        }
    }
    Ok(())
}

pub fn show(store: &NoteStore, id: String, as_json: bool) -> Result<()> {
    let note = store
        .get_note(&id)?
        .with_context(|| format!("note not found: {id}"))?;
    let version = store.note_version(&id).map_err(not_found_context(&id))?;
    if as_json {
        let mut value = serde_json::to_value(&note.metadata)?;
        value["body"] = json!(note.body.as_str());
        // `version` is the token to pass to `edit-lines --expect` so an
        // edit is rejected if the note changed since this read.
        value["version"] = json!(version);
        print_json(&value)?;
    } else {
        let m = &note.metadata;
        println!("id:       {}", m.id);
        println!("created:  {}", m.created_at);
        println!("modified: {}", m.modified_at);
        println!("tags:     {}", m.tags.join(", "));
        println!("version:  {version}");
        println!();
        // 1-based line numbers so `edit-lines <id> <start> <end>` can
        // address a line without the reader counting by hand.
        for (i, line) in note.body.as_str().split('\n').enumerate() {
            println!("{:>4}  {line}", i + 1);
        }
    }
    Ok(())
}

pub fn edit(store: &mut NoteStore, id: String, body: Option<String>, as_json: bool) -> Result<()> {
    let body = body
        .or_else(read_stdin)
        .context("provide --body or pipe content on stdin")?;
    let meta = store
        .update_note(&id, &body)
        .map_err(not_found_context(&id))?;
    if as_json {
        print_json(&serde_json::to_value(&meta)?)?;
    } else {
        println!("Updated: {} ({})", display_title(&meta), meta.id);
    }
    Ok(())
}

pub fn edit_lines(store: &mut NoteStore, id: String, start: usize, end: usize, text: Option<String>, expect: Option<String>, as_json: bool) -> Result<()> {
    let text = text.or_else(read_stdin).unwrap_or_default();
    let meta = store
        .edit_lines(&id, expect.as_deref(), start, end, &text)
        .map_err(not_found_context(&id))?;
    if as_json {
        print_json(&serde_json::to_value(&meta)?)?;
    } else {
        println!(
            "Edited lines {start}..={end} of {} ({})",
            display_title(&meta),
            meta.id
        );
    }
    Ok(())
}

pub fn search(store: &NoteStore, query: String, limit: usize, as_json: bool) -> Result<()> {
    let results = store.search(&query, limit)?;
    if as_json {
        print_json(&serde_json::to_value(&results)?)?;
    } else {
        for r in &results {
            let title = if r.title.is_empty() {
                "(untitled)"
            } else {
                &r.title
            };
            let snippet = r.snippet.split_whitespace().collect::<Vec<_>>().join(" ");
            println!("{}  {title} — {snippet}", r.note_id);
        }
    }
    Ok(())
}

pub fn tags(store: &NoteStore, as_json: bool) -> Result<()> {
    let tags = store.list_tags()?;
    if as_json {
        let value: Vec<_> = tags
            .iter()
            .map(|(tag, count)| json!({"tag": tag, "count": count}))
            .collect();
        print_json(&serde_json::to_value(value)?)?;
    } else {
        for (tag, count) in &tags {
            println!("{tag} ({count})");
        }
    }
    Ok(())
}

pub fn delete(store: &mut NoteStore, id: String, as_json: bool) -> Result<()> {
    let meta = store.delete_note(&id).map_err(not_found_context(&id))?;
    if as_json {
        print_json(&json!({"id": meta.id, "deleted": true}))?;
    } else {
        println!("Deleted: {} ({})", display_title(&meta), meta.id);
    }
    Ok(())
}

pub fn add(store: &mut NoteStore, data_dir: &Path, action: NoteAction, as_json: bool) -> Result<()> {
    match action {
        NoteAction::Add {
            text,
            file,
            project: project_override,
            note_type,
        } => {
            let text = match (text, file) {
                (Some(t), _) => t,
                (None, Some(path)) => std::fs::read_to_string(&path)
                    .with_context(|| format!("reading note body from {}", path.display()))?,
                (None, None) => read_stdin()
                    .context("provide note text, --file <path>, or pipe content on stdin")?,
            };
            let cwd = std::env::current_dir().context("reading current directory")?;
            let tag = project::resolve(&cwd, project_override.as_deref())?;
            let body = project::ensure_tag(&text, &tag);
            let meta = store.create_note_with_type(
                &body,
                &author(data_dir)?,
                note_type.as_deref().unwrap_or_default(),
            )?;
            if as_json {
                print_json(&serde_json::to_value(&meta)?)?;
            } else {
                println!("Added to {tag}: {} ({})", display_title(&meta), meta.id);
            }
        }
        NoteAction::SetType { note_id, note_type } => {
            let meta = store
                .set_note_type(&note_id, &note_type)
                .map_err(not_found_context(&note_id))?;
            if as_json {
                print_json(&serde_json::to_value(&meta)?)?;
            } else {
                println!("Set {} to type {}", meta.id, meta.note_type);
            }
        }
    }
    Ok(())
}

pub fn list_project(store: &NoteStore, project_override: Option<String>, note_type: Option<String>, as_json: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let tag = project::resolve(&cwd, project_override.as_deref())?;
    let notes = match &note_type {
        Some(t) => store.list_by_tag_and_type(&tag, t)?,
        None => store.list_by_tag(&tag)?,
    };
    if as_json {
        print_json(&serde_json::to_value(&notes)?)?;
    } else {
        for m in &notes {
            println!(
                "{}  {}  {}{}",
                m.id,
                m.modified_at,
                display_title(m),
                tag_suffix(m)
            );
        }
    }
    Ok(())
}
