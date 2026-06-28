//! Kiem CLI: human- and agent-facing interface over `kiem-core`.
//!
//! Every command supports `--json` for structured output (the agent surface);
//! the default output is human-readable. Note bodies come from `--body` or
//! stdin (pipe-friendly). Titles are never set directly — they derive from
//! the body per the content contract, so `--title` is sugar that prepends an
//! H1 heading line.

mod daemon;
mod project;

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use kiem_core::note::NoteMetadata;
use kiem_core::store::NoteStore;
use serde_json::json;

/// Stand-in author until the identity module (U11) provides real DIDs.
const AUTHOR_PLACEHOLDER: &str = "local";

#[derive(Parser)]
#[command(name = "kiem", version, about = "Kiem: P2P notes for humans and agents")]
struct Cli {
    /// Data directory (default: ~/.kiem)
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Structured JSON output
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a note from --body and/or stdin; --title prepends an H1 heading
    Create {
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        body: Option<String>,
    },
    /// List notes, most recently modified first
    List {
        /// Only notes carrying this exact tag
        #[arg(long)]
        tag: Option<String>,
    },
    /// Show one note (metadata + body)
    Show { id: String },
    /// Replace a note's body from --body or stdin
    Edit {
        id: String,
        #[arg(long)]
        body: Option<String>,
    },
    /// Full-text search over titles, bodies, and tags
    Search {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List all tags with usage counts
    Tags,
    /// Move a note to trash (soft delete)
    Delete { id: String },
    /// Manage projects (a project is the reserved tag proj/<slug>)
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// List the current project's open todos (note-id, index, text)
    Todos {
        /// Override the resolved project (a name or proj/<slug>)
        #[arg(long)]
        project: Option<String>,
    },
    /// Check or uncheck a todo by its (note-id, index) address
    Todo {
        #[command(subcommand)]
        action: TodoAction,
    },
    /// Add a note to the current project
    Note {
        #[command(subcommand)]
        action: NoteAction,
    },
    /// List the current project's notes
    Notes {
        /// Override the resolved project (a name or proj/<slug>)
        #[arg(long)]
        project: Option<String>,
    },
    /// Run the sync daemon (foreground): discover peers, keep notes converged
    Sync {
        /// Listen address (port 0 = ephemeral)
        #[arg(long, default_value = "0.0.0.0:0")]
        listen: String,
        /// Direct peer address to dial (repeatable); used by tests and
        /// fixed-address setups like a home server
        #[arg(long)]
        connect: Vec<String>,
        /// Disable mDNS discovery (direct connections only)
        #[arg(long)]
        no_mdns: bool,
        /// Sync round interval in milliseconds
        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,
    },
    /// Show the running daemon's peers and state
    SyncStatus,
}

#[derive(Subcommand)]
enum ProjectAction {
    /// Register the current directory as a project: write the .kiem marker, add an
    /// AGENTS.md pointer, and (for a new project) create a home note
    Add { name: String },
    /// List known projects (derived from proj/* tags with note counts)
    List,
    /// Print the project resolved for the current directory
    Current,
}

#[derive(Subcommand)]
enum TodoAction {
    /// Mark a todo done: kiem todo check <note-id> <index>
    Check { note_id: String, index: usize },
    /// Mark a todo not done: kiem todo uncheck <note-id> <index>
    Uncheck { note_id: String, index: usize },
}

#[derive(Subcommand)]
enum NoteAction {
    /// Add a note to the current project (tags it proj/<slug>)
    Add {
        /// Note text; the first line becomes the title
        text: String,
        /// Override the resolved project (a name or proj/<slug>)
        #[arg(long)]
        project: Option<String>,
    },
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
    // The daemon owns its store (long-lived); sync-status needs none.
    match cli.command {
        Command::Sync { listen, connect, no_mdns, interval_ms } => {
            return daemon::run(daemon::Options {
                data_dir,
                listen,
                connect,
                mdns: !no_mdns,
                interval: std::time::Duration::from_millis(interval_ms.max(100)),
            });
        }
        Command::SyncStatus => return daemon::print_status(&data_dir, cli.json),
        _ => {}
    }

    let mut store = NoteStore::open_dir(&data_dir)
        .with_context(|| format!("opening data directory {}", data_dir.display()))?;

    match cli.command {
        Command::Create { title, body } => {
            let body = compose_body(title, body)?;
            let meta = store.create_note(&body, AUTHOR_PLACEHOLDER)?;
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
            if cli.json {
                let mut value = serde_json::to_value(&note.metadata)?;
                value["body"] = json!(note.body.as_str());
                print_json(&value)?;
            } else {
                let m = &note.metadata;
                println!("id:       {}", m.id);
                println!("created:  {}", m.created_at);
                println!("modified: {}", m.modified_at);
                println!("tags:     {}", m.tags.join(", "));
                println!();
                println!("{}", note.body.as_str());
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
                    Some(store.create_note(&body, AUTHOR_PLACEHOLDER)?)
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
                if cli.json {
                    print_json(&json!({"project": tag}))?;
                } else {
                    println!("{tag}");
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
        Command::Todo { action } => {
            let (note_id, index, checked) = match action {
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
            NoteAction::Add { text, project: project_override } => {
                let cwd = std::env::current_dir().context("reading current directory")?;
                let tag = project::resolve(&cwd, project_override.as_deref())?;
                let body = format!("{text}\n\n#{tag}");
                let meta = store.create_note(&body, AUTHOR_PLACEHOLDER)?;
                if cli.json {
                    print_json(&serde_json::to_value(&meta)?)?;
                } else {
                    println!("Added to {tag}: {} ({})", display_title(&meta), meta.id);
                }
            }
        },
        Command::Notes { project: project_override } => {
            let cwd = std::env::current_dir().context("reading current directory")?;
            let tag = project::resolve(&cwd, project_override.as_deref())?;
            let notes = store.list_by_tag(&tag)?;
            if cli.json {
                print_json(&serde_json::to_value(&notes)?)?;
            } else {
                for m in &notes {
                    println!("{}  {}  {}{}", m.id, m.modified_at, display_title(m), tag_suffix(m));
                }
            }
        }
        Command::Sync { .. } | Command::SyncStatus => unreachable!("handled above"),
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
