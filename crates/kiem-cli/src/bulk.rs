//! Safe multi-note mutations: explicit selection, preview, confirmation, and
//! one atomic store transaction.

use std::collections::HashSet;
use std::io::Read;

use anyhow::{bail, Context, Result};
use kiem_core::content;
use kiem_core::note::{NoteMetadata, DEFAULT_NOTE_TYPE};
use kiem_core::store::NoteStore;
use serde_json::json;

use crate::args::{BulkAction, BulkArgs, BulkTagAction};
use crate::project;

struct SelectedNote {
    metadata: NoteMetadata,
    version: String,
}

enum Selector {
    Tag(String),
    Ids(Vec<String>),
}

enum Operation {
    AddTag(String),
    RemoveTag(String),
    SetType(String),
    Delete,
    Restore,
}

pub fn run(store: &mut NoteStore, args: BulkArgs, json_output: bool) -> Result<()> {
    let BulkArgs {
        tag,
        project,
        ids,
        stdin,
        dry_run,
        yes,
        action,
    } = args;
    if !dry_run && !yes {
        bail!("bulk mutation requires --dry-run or --yes");
    }

    let operation = operation(action)?;
    let selector = selector(tag, project, ids, stdin)?;
    let restoring = matches!(&operation, Operation::Restore);

    if dry_run {
        let selected = select(store, &selector, restoring)?;
        let change_ids = change_ids(&selected, &operation);
        print_summary(
            json_output,
            true,
            operation.label(),
            selected.len(),
            &change_ids,
        )?;
        return Ok(());
    }

    let (selected_count, change_ids) = store.bulk(|store| -> Result<(usize, Vec<String>)> {
        // Selection and version capture happen under the same IMMEDIATE
        // transaction that applies the operation, so selector membership
        // cannot go stale while another process writes.
        let selected = select(store, &selector, restoring)?;
        let change_ids = change_ids(&selected, &operation);
        for note in &selected {
            let found = store.note_version(&note.metadata.id)?;
            if found != note.version {
                bail!(
                    "note {} changed since bulk selection; no notes changed",
                    note.metadata.id
                );
            }
        }
        for note in &selected {
            if needs_change(&note.metadata, &operation) {
                apply(store, &note.metadata.id, &operation)?;
            }
        }
        Ok((selected.len(), change_ids))
    })?;

    print_summary(
        json_output,
        false,
        operation.label(),
        selected_count,
        &change_ids,
    )
}

fn operation(action: BulkAction) -> Result<Operation> {
    Ok(match action {
        BulkAction::Tag {
            action: BulkTagAction::Add { tag },
        } => Operation::AddTag(require_tag(&tag)?),
        BulkAction::Tag {
            action: BulkTagAction::Remove { tag },
        } => Operation::RemoveTag(require_tag(&tag)?),
        BulkAction::SetType { note_type } => Operation::SetType(normalize_type(&note_type)),
        BulkAction::Delete => Operation::Delete,
        BulkAction::Restore => Operation::Restore,
    })
}

fn selector(
    tag: Option<String>,
    project_name: Option<String>,
    ids: Vec<String>,
    stdin: bool,
) -> Result<Selector> {
    let choices = usize::from(tag.is_some())
        + usize::from(project_name.is_some())
        + usize::from(!ids.is_empty())
        + usize::from(stdin);
    if choices != 1 {
        bail!("choose exactly one selector: --tag, --project, one or more --id, or --stdin");
    }
    if let Some(tag) = tag {
        return Ok(Selector::Tag(require_tag(&tag)?));
    }
    if let Some(name) = project_name {
        return Ok(Selector::Tag(project::require_tag(&name)?));
    }
    if stdin {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .context("reading note IDs from stdin")?;
        return ids_selector(input.lines().map(str::to_owned).collect());
    }
    ids_selector(ids)
}

fn ids_selector(ids: Vec<String>) -> Result<Selector> {
    let mut seen = HashSet::new();
    let ids: Vec<_> = ids
        .into_iter()
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect();
    if ids.is_empty() {
        bail!("the ID selector is empty");
    }
    Ok(Selector::Ids(ids))
}

fn select(store: &NoteStore, selector: &Selector, deleted: bool) -> Result<Vec<SelectedNote>> {
    let metadata = match selector {
        Selector::Tag(tag) if deleted => store.list_deleted_by_tag(tag)?,
        Selector::Tag(tag) => store.list_by_tag(tag)?,
        Selector::Ids(ids) => ids
            .iter()
            .map(|id| {
                store
                    .get_note(id)?
                    .map(|note| note.metadata)
                    .with_context(|| format!("note not found: {id}"))
            })
            .collect::<Result<Vec<_>>>()?,
    };
    metadata
        .into_iter()
        .map(|metadata| {
            let version = store.note_version(&metadata.id)?;
            Ok(SelectedNote { metadata, version })
        })
        .collect()
}

fn change_ids(selected: &[SelectedNote], operation: &Operation) -> Vec<String> {
    selected
        .iter()
        .filter(|note| needs_change(&note.metadata, operation))
        .map(|note| note.metadata.id.clone())
        .collect()
}

fn needs_change(note: &NoteMetadata, operation: &Operation) -> bool {
    match operation {
        Operation::AddTag(tag) => !note.tags.contains(tag),
        Operation::RemoveTag(tag) => note.tags.contains(tag),
        Operation::SetType(note_type) => note.note_type != *note_type,
        Operation::Delete => !note.deleted,
        Operation::Restore => note.deleted,
    }
}

fn apply(store: &mut NoteStore, id: &str, operation: &Operation) -> Result<()> {
    match operation {
        Operation::AddTag(tag) => store.add_tag(id, tag)?,
        Operation::RemoveTag(tag) => store.remove_tag(id, tag)?,
        Operation::SetType(note_type) => store.set_note_type(id, note_type)?,
        Operation::Delete => store.delete_note(id)?,
        Operation::Restore => store.restore_note(id)?,
    };
    Ok(())
}

fn require_tag(raw: &str) -> Result<String> {
    let tag = raw.trim().strip_prefix('#').unwrap_or(raw.trim());
    let parsed = content::extract_tags(&format!("#{tag}"));
    if parsed.len() != 1 || parsed[0] != tag {
        bail!("invalid tag {raw:?}");
    }
    Ok(tag.to_owned())
}

fn normalize_type(note_type: &str) -> String {
    let note_type = note_type.trim();
    if note_type.is_empty() {
        DEFAULT_NOTE_TYPE
    } else {
        note_type
    }
    .to_owned()
}

fn print_summary(
    json_output: bool,
    dry_run: bool,
    action: String,
    selected: usize,
    changed_ids: &[String],
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "action": action,
                "dry_run": dry_run,
                "selected": selected,
                "would_change": if dry_run { changed_ids.len() } else { 0 },
                "changed": if dry_run { 0 } else { changed_ids.len() },
                "unchanged": selected - changed_ids.len(),
                "ids": changed_ids,
                "failed": [],
            }))?
        );
    } else if dry_run {
        println!(
            "Dry run: {} of {selected} note(s) would change ({} unchanged)\nAction: {action}\nRun again with --yes to apply.",
            changed_ids.len(),
            selected - changed_ids.len(),
        );
    } else {
        println!(
            "Changed {} of {selected} note(s) ({} unchanged)\nAction: {action}",
            changed_ids.len(),
            selected - changed_ids.len(),
        );
    }
    Ok(())
}

impl Operation {
    fn label(&self) -> String {
        match self {
            Self::AddTag(tag) => format!("tag add {tag}"),
            Self::RemoveTag(tag) => format!("tag remove {tag}"),
            Self::SetType(note_type) => format!("set-type {note_type}"),
            Self::Delete => "delete".to_owned(),
            Self::Restore => "restore".to_owned(),
        }
    }
}
