//! Kiem CLI: human- and agent-facing interface over `kiem-core`.
//!
//! Every command supports `--json` for structured output (the agent surface);
//! the default output is human-readable. Note bodies come from `--body` or
//! stdin (pipe-friendly). Titles are never set directly — they derive from
//! the body per the content contract, so `--title` is sugar that prepends an
//! H1 heading line.

mod args;
mod control;
mod daemon;
mod pair;
mod project;

use std::io::{IsTerminal, Read};
use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::Parser;

use args::{Cli, Command, NoteAction, PairAction, ProjectAction, TodoAction};
use kiem_core::note::NoteMetadata;
use kiem_core::store::NoteStore;
use serde_json::json;

/// Note authorship: this device's iroh `EndpointId` (the persisted identity
/// in the data dir, created on first use) — the same id peers see on the mesh.
fn author(data_dir: &Path) -> Result<String> {
    Ok(kiem_sync::device_id(data_dir)?.to_string())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("kiem: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = match &cli.data_dir {
        Some(dir) => dir.clone(),
        None => std::env::home_dir()
            .context("cannot determine home directory; pass --data-dir")?
            .join(".kiem"),
    };
    // The daemon owns its store (long-lived); sync-status/pair need none.
    match cli.command {
        Command::Sync { interval_ms } => {
            return daemon::run(daemon::Options {
                data_dir,
                interval: std::time::Duration::from_millis(interval_ms.max(100)),
            });
        }
        Command::SyncStatus => return daemon::print_status(&data_dir, cli.json),
        Command::Pair { action } => {
            let runtime = tokio::runtime::Runtime::new().context("starting async runtime")?;
            return match action {
                PairAction::Show { yes } => runtime.block_on(pair::show(&data_dir, yes, cli.json)),
                PairAction::Add { ticket } => {
                    runtime.block_on(pair::add(&data_dir, &ticket, cli.json))
                }
            };
        }
        _ => {}
    }

    let mut store = NoteStore::open_dir(&data_dir)
        .with_context(|| format!("opening data directory {}", data_dir.display()))?;

    match cli.command {
        Command::Create { title, body } => {
            let body = compose_body(title, body)?;
            let meta = store.create_note(&body, &author(&data_dir)?)?;
            if cli.json {
                print_json(&serde_json::to_value(&meta)?)?;
            } else {
                println!("Created: {} ({})", display_title(&meta), meta.id);
            }
        }
        Command::List { tag } => {
            let notes = match tag {
                Some(tag) => store.list_by_tag(&tag)?,
                None => store.list_notes()?,
            };
            if cli.json {
                print_json(&serde_json::to_value(&notes)?)?;
            } else {
                for m in &notes {
                    println!("{}  {}  {}{}", m.id, m.modified_at, display_title(m), tag_suffix(m));
                }
            }
        }
        Command::Show { id } => {
            let note = store.get_note(&id)?.with_context(|| format!("note not found: {id}"))?;
            let version = store.note_version(&id).map_err(not_found_context(&id))?;
            if cli.json {
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
        }
        Command::Edit { id, body } => {
            let body = body
                .or_else(read_stdin)
                .context("provide --body or pipe content on stdin")?;
            let meta = store.update_note(&id, &body).map_err(not_found_context(&id))?;
            if cli.json {
                print_json(&serde_json::to_value(&meta)?)?;
            } else {
                println!("Updated: {} ({})", display_title(&meta), meta.id);
            }
        }
        Command::EditLines { id, start, end, text, expect } => {
            let text = text.or_else(read_stdin).unwrap_or_default();
            let meta = store
                .edit_lines(&id, expect.as_deref(), start, end, &text)
                .map_err(not_found_context(&id))?;
            if cli.json {
                print_json(&serde_json::to_value(&meta)?)?;
            } else {
                println!("Edited lines {start}..={end} of {} ({})", display_title(&meta), meta.id);
            }
        }
        Command::Search { query, limit } => {
            let results = store.search(&query, limit)?;
            if cli.json {
                print_json(&serde_json::to_value(&results)?)?;
            } else {
                for r in &results {
                    let title = if r.title.is_empty() { "(untitled)" } else { &r.title };
                    let snippet = r.snippet.split_whitespace().collect::<Vec<_>>().join(" ");
                    println!("{}  {title} — {snippet}", r.note_id);
                }
            }
        }
        Command::Tags => {
            let tags = store.list_tags()?;
            if cli.json {
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
        }
        Command::Delete { id } => {
            let meta = store.delete_note(&id).map_err(not_found_context(&id))?;
            if cli.json {
                print_json(&json!({"id": meta.id, "deleted": true}))?;
            } else {
                println!("Deleted: {} ({})", display_title(&meta), meta.id);
            }
        }
        Command::Project { action } => match action {
            ProjectAction::Add { name } => {
                let tag = project::to_tag(&name);
                if tag.is_empty() {
                    bail!("cannot derive a project name from {name:?}");
                }
                let cwd = std::env::current_dir().context("reading current directory")?;
                let marker = project::write_marker(&cwd, &tag)?;
                project::ensure_agents_pointer(&cwd, &tag)?;
                // Create a home note only if this project tag is new, so `add`
                // is idempotent (re-binding an existing project just rewrites the marker).
                let created = if store.list_by_tag(&tag)?.is_empty() {
                    let body = format!("# {name}\n\nProject home.\n\n#{tag}");
                    Some(store.create_note(&body, &author(&data_dir)?)?)
                } else {
                    None
                };
                if cli.json {
                    print_json(&json!({
                        "project": tag,
                        "marker": marker.display().to_string(),
                        "home_note": created.as_ref().map(|m| m.id.clone()),
                    }))?;
                } else {
                    println!("Project {tag}");
                    println!("  marker: {}", marker.display());
                    match &created {
                        Some(m) => println!("  home note: {}", m.id),
                        None => println!("  (existing project — bound this directory)"),
                    }
                }
            }
            ProjectAction::List => {
                let projects: Vec<_> = store
                    .list_tags()?
                    .into_iter()
                    .filter(|(tag, _)| tag.starts_with(project::TAG_PREFIX))
                    .collect();
                if cli.json {
                    let value: Vec<_> = projects
                        .iter()
                        .map(|(tag, notes)| json!({"project": tag, "notes": notes}))
                        .collect();
                    print_json(&serde_json::to_value(value)?)?;
                } else if projects.is_empty() {
                    println!("(no projects yet — create one with `kiem project add <name>`)");
                } else {
                    for (tag, notes) in &projects {
                        println!("{tag} ({notes})");
                    }
                }
            }
            ProjectAction::Current => {
                let cwd = std::env::current_dir().context("reading current directory")?;
                let tag = project::resolve(&cwd, None)?;
                // `resolve` always succeeds via the directory-name fallback, so
                // success alone doesn't mean the repo is onboarded — check the
                // marker directly and say so explicitly.
                let onboarded = project::read_marker(&cwd)?.is_some();
                if cli.json {
                    print_json(&json!({"project": tag, "onboarded": onboarded}))?;
                } else {
                    println!("{tag}");
                    if !onboarded {
                        eprintln!(
                            "(no committed .kiem marker — this is a directory-name guess, \
                             not an onboarded project; run `kiem project add` to onboard)"
                        );
                    }
                }
            }
        },
        Command::Todos { project: project_override } => {
            let cwd = std::env::current_dir().context("reading current directory")?;
            let tag = project::resolve(&cwd, project_override.as_deref())?;
            let todos = store.list_todo_items_for_tag(&tag)?;
            if cli.json {
                print_json(&serde_json::to_value(&todos)?)?;
            } else if todos.is_empty() {
                println!("(no open todos in {tag})");
            } else {
                for t in &todos {
                    println!("{}  {}  {}", t.note_id, t.index, t.text);
                }
            }
        }
        Command::Todo { action: TodoAction::Add { note_id, text } } => {
            if text.trim().is_empty() {
                bail!("todo text is empty");
            }
            let meta = store.add_todo(&note_id, &text).map_err(not_found_context(&note_id))?;
            if cli.json {
                print_json(&serde_json::to_value(&meta)?)?;
            } else {
                println!("Added todo to {} ({})", display_title(&meta), meta.id);
            }
        }
        Command::Todo { action } => {
            let (note_id, index, checked) = match action {
                TodoAction::Add { .. } => unreachable!("handled above"),
                TodoAction::Check { note_id, index } => (note_id, index, true),
                TodoAction::Uncheck { note_id, index } => (note_id, index, false),
            };
            let meta = store
                .set_todo_checked(&note_id, index, checked)
                .map_err(not_found_context(&note_id))?;
            if cli.json {
                print_json(&json!({"id": meta.id, "index": index, "checked": checked}))?;
            } else {
                let verb = if checked { "Checked" } else { "Unchecked" };
                println!("{verb} todo {index} in {} ({})", display_title(&meta), meta.id);
            }
        }
        Command::Note { action } => match action {
            NoteAction::Add { text, file, project: project_override, note_type } => {
                let text = match (text, file) {
                    (Some(t), _) => t,
                    (None, Some(path)) => std::fs::read_to_string(&path)
                        .with_context(|| format!("reading note body from {}", path.display()))?,
                    (None, None) => read_stdin()
                        .context("provide note text, --file <path>, or pipe content on stdin")?,
                };
                let cwd = std::env::current_dir().context("reading current directory")?;
                let tag = project::resolve(&cwd, project_override.as_deref())?;
                // Only auto-append the project tag if the text doesn't already
                // carry it — otherwise a hand-tagged note ends up with it twice.
                let body = if kiem_core::content::extract_tags(&text).contains(&tag) {
                    text
                } else {
                    format!("{text}\n\n#{tag}")
                };
                let meta = store.create_note_with_type(
                    &body,
                    &author(&data_dir)?,
                    note_type.as_deref().unwrap_or_default(),
                )?;
                if cli.json {
                    print_json(&serde_json::to_value(&meta)?)?;
                } else {
                    println!("Added to {tag}: {} ({})", display_title(&meta), meta.id);
                }
            }
            NoteAction::SetType { note_id, note_type } => {
                let meta = store.set_note_type(&note_id, &note_type).map_err(not_found_context(&note_id))?;
                if cli.json {
                    print_json(&serde_json::to_value(&meta)?)?;
                } else {
                    println!("Set {} to type {}", meta.id, meta.note_type);
                }
            }
        },
        Command::Notes { project: project_override, note_type } => {
            let cwd = std::env::current_dir().context("reading current directory")?;
            let tag = project::resolve(&cwd, project_override.as_deref())?;
            let notes = match &note_type {
                Some(t) => store.list_by_tag_and_type(&tag, t)?,
                None => store.list_by_tag(&tag)?,
            };
            if cli.json {
                print_json(&serde_json::to_value(&notes)?)?;
            } else {
                for m in &notes {
                    println!("{}  {}  {}{}", m.id, m.modified_at, display_title(m), tag_suffix(m));
                }
            }
        }
        Command::Sync { .. } | Command::SyncStatus | Command::Pair { .. } => {
            unreachable!("handled above")
        }
    }
    Ok(())
}

/// Body from explicit flags and/or stdin. `--title` prepends an H1 so the
/// derived title matches what the user asked for.
fn compose_body(title: Option<String>, body: Option<String>) -> Result<String> {
    let body = body.or_else(read_stdin);
    match (title, body) {
        (Some(t), Some(b)) => Ok(format!("# {t}\n\n{b}")),
        (Some(t), None) => Ok(format!("# {t}")),
        (None, Some(b)) => Ok(b),
        (None, None) => bail!("provide --body, --title, or pipe content on stdin"),
    }
}

fn read_stdin() -> Option<String> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut buf = String::new();
    stdin.read_to_string(&mut buf).ok()?;
    let trimmed = buf.trim_end();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn display_title(m: &NoteMetadata) -> &str {
    if m.title.is_empty() {
        "(untitled)"
    } else {
        &m.title
    }
}

fn tag_suffix(m: &NoteMetadata) -> String {
    if m.tags.is_empty() {
        String::new()
    } else {
        format!("  [{}]", m.tags.join(", "))
    }
}

fn not_found_context(id: &str) -> impl FnOnce(kiem_core::store::StoreError) -> anyhow::Error + '_ {
    move |err| match err {
        kiem_core::store::StoreError::NotFound(_) => anyhow::anyhow!("note not found: {id}"),
        other => other.into(),
    }
}

fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
