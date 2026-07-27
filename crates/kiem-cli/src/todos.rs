//! The todo commands. A todo is a `- [ ]` line in a note body, addressed by
//! its position among all checkbox lines — including checked ones, so
//! checking one item never renumbers the others.

use anyhow::{bail, Context, Result};
use kiem_core::store::NoteStore;
use serde_json::json;

use crate::args::TodoAction;
use crate::output::{display_title, not_found_context, print_json};
use crate::project;

pub fn list(store: &NoteStore, project_override: Option<String>, as_json: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let tag = project::resolve(&cwd, project_override.as_deref())?;
    let todos = store.list_todo_items_for_tag(&tag)?;
    if as_json {
        print_json(&serde_json::to_value(&todos)?)?;
    } else if todos.is_empty() {
        println!("(no open todos in {tag})");
    } else {
        for t in &todos {
            println!("{}  {}  {}", t.note_id, t.index, t.text);
        }
    }
    Ok(())
}

pub fn add(store: &mut NoteStore, note_id: String, text: String, as_json: bool) -> Result<()> {
    if text.trim().is_empty() {
        bail!("todo text is empty");
    }
    let meta = store
        .add_todo(&note_id, &text)
        .map_err(not_found_context(&note_id))?;
    if as_json {
        print_json(&serde_json::to_value(&meta)?)?;
    } else {
        println!("Added todo to {} ({})", display_title(&meta), meta.id);
    }
    Ok(())
}

pub fn set(store: &mut NoteStore, action: TodoAction, as_json: bool) -> Result<()> {
    let (note_id, indices, checked) = match action {
        TodoAction::Add { .. } => unreachable!("handled above"),
        TodoAction::Check { note_id, indices } => (note_id, indices, true),
        TodoAction::Uncheck { note_id, indices } => (note_id, indices, false),
    };
    let meta = store
        .set_todos_checked(&note_id, &indices, checked)
        .map_err(not_found_context(&note_id))?;
    if as_json {
        print_json(&json!({"id": meta.id, "indices": indices, "checked": checked}))?;
    } else {
        let verb = if checked { "Checked" } else { "Unchecked" };
        let noun = if indices.len() == 1 { "todo" } else { "todos" };
        let positions = indices
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{verb} {noun} {positions} in {} ({})",
            display_title(&meta),
            meta.id
        );
    }
    Ok(())
}
