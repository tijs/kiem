//! Kiem CLI: human- and agent-facing interface over `kiem-core`.
//!
//! Every command supports `--json` for structured output (the agent surface);
//! the default output is human-readable. Note bodies come from `--body` or
//! stdin (pipe-friendly). Titles are never set directly — they derive from
//! the body per the content contract, so `--title` is sugar that prepends an
//! H1 heading line.
//!
//! This file is dispatch only: parse, resolve the data dir, open the store,
//! hand off. The handlers live per concern — [`notes`], [`todos`],
//! [`transfer`], [`bulk`], [`pair`], [`daemon`] — and shape their output
//! through [`output`].

mod args;
mod bulk;
mod control;
mod daemon;
mod notes;
mod output;
mod pair;
mod project;
mod todos;
mod transfer;

use std::path::Path;

use anyhow::{Context, Result};
use clap::Parser;

use args::{Cli, Command, PairAction, ProjectAction, TodoAction};
use kiem_core::store::NoteStore;
use output::print_json;
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
                PairAction::List => pair::list(&data_dir, cli.json),
                PairAction::Forget { peer_id } => {
                    runtime.block_on(pair::forget(&data_dir, &peer_id, cli.json))
                }
            };
        }
        _ => {}
    }

    let mut store = NoteStore::open_dir(&data_dir)
        .with_context(|| format!("opening data directory {}", data_dir.display()))?;

    match cli.command {
        Command::Create { title, body } => {
            notes::create(&mut store, &data_dir, title, body, cli.json)?
        }
        Command::List { tag } => notes::list(&store, tag, cli.json)?,
        Command::Show { id } => notes::show(&store, id, cli.json)?,
        Command::Edit { id, body } => notes::edit(&mut store, id, body, cli.json)?,
        Command::EditLines {
            id,
            start,
            end,
            text,
            expect,
        } => notes::edit_lines(&mut store, id, start, end, text, expect, cli.json)?,
        Command::Search { query, limit } => notes::search(&store, query, limit, cli.json)?,
        Command::Tags => notes::tags(&store, cli.json)?,
        Command::Delete { id } => notes::delete(&mut store, id, cli.json)?,
        Command::Note { action } => notes::add(&mut store, &data_dir, action, cli.json)?,
        Command::Notes {
            project,
            note_type,
        } => notes::list_project(&store, project, note_type, cli.json)?,
        Command::Todos { project } => todos::list(&store, project, cli.json)?,
        Command::Todo {
            action: TodoAction::Add { note_id, text },
        } => todos::add(&mut store, note_id, text, cli.json)?,
        Command::Todo { action } => todos::set(&mut store, action, cli.json)?,
        Command::Bulk(args) => bulk::run(&mut store, args, cli.json)?,
        Command::Project { action } => project_cmd(&mut store, &data_dir, action, cli.json)?,
        Command::Export { dir, project } => transfer::export(&store, dir, project, cli.json)?,
        Command::Import {
            dir,
            project,
            no_project,
        } => transfer::import(&mut store, &data_dir, dir, project, no_project, cli.json)?,
        Command::Sync { .. } | Command::SyncStatus | Command::Pair { .. } => {
            unreachable!("handled above")
        }
    }
    Ok(())
}

/// The `kiem project` commands. The heavy lifting is in `project.rs`;
/// this is dispatch and output.
fn project_cmd(
    store: &mut NoteStore,
    data_dir: &Path,
    action: ProjectAction,
    as_json: bool,
) -> Result<()> {
    match action {
    ProjectAction::Add { name } => {
        let tag = project::require_tag(&name)?;
        let cwd = std::env::current_dir().context("reading current directory")?;
        let marker = project::write_marker(&cwd, &tag)?;
        project::ensure_agents_pointer(&cwd, &tag)?;
        // Create a home note only if this project tag is new, so `add`
        // is idempotent (re-binding an existing project just rewrites the marker).
        let created = if store.list_by_tag(&tag)?.is_empty() {
            let body = format!("# {name}\n\nProject home.\n\n#{tag}");
            Some(store.create_note(&body, &author(data_dir)?)?)
        } else {
            None
        };
        if as_json {
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
        if as_json {
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
        if as_json {
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
    }
    Ok(())
}
