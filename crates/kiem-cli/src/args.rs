//! Clap definitions for the `kiem` CLI — every command, flag, and
//! subcommand enum. Dispatch and handlers live in `main.rs`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Accept a bare note id or a `kiem://note/<id>` reference, returning the id.
fn note_ref(s: &str) -> Result<String, String> {
    let s = s.strip_prefix("kiem://note/").unwrap_or(s);
    Ok(s.trim_end_matches('/').to_owned())
}

#[derive(Parser)]
#[command(
    name = "kiem",
    version,
    about = "Kiem: P2P notes for humans and agents"
)]
pub struct Cli {
    /// Data directory (default: ~/.kiem)
    #[arg(long, global = true, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,

    /// Structured JSON output
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
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
    Show {
        #[arg(value_parser = note_ref)]
        id: String,
    },
    /// Replace a note's body from --body or stdin
    Edit {
        #[arg(value_parser = note_ref)]
        id: String,
        #[arg(long)]
        body: Option<String>,
    },
    /// Replace a 1-based inclusive line range with --text or stdin (a targeted,
    /// scalar-safe edit). Pass --expect <version> from `show` to reject the edit
    /// if the note changed since you read it.
    EditLines {
        #[arg(value_parser = note_ref)]
        id: String,
        /// First line to replace (1-based, inclusive).
        start: usize,
        /// Last line to replace (1-based, inclusive; equals start for one line).
        end: usize,
        /// Replacement text (may be multi-line); empty deletes the range.
        /// Hyphen-led values are fine (todo lines start with `- `).
        #[arg(long, allow_hyphen_values = true)]
        text: Option<String>,
        /// Reject unless the note's current version matches (from `show`).
        #[arg(long)]
        expect: Option<String>,
    },
    /// Full-text search over titles, bodies, and tags
    Search {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List all tags with usage counts
    Tags,
    /// Apply one operation to multiple notes selected by tag, project, IDs, or stdin
    Bulk(BulkArgs),
    /// Move a note to trash (soft delete)
    Delete {
        #[arg(value_parser = note_ref)]
        id: String,
    },
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
        /// Only notes of this kind (e.g. plan, brainstorm, review, solution)
        #[arg(long = "type")]
        note_type: Option<String>,
    },
    /// Export notes as a directory of Markdown files — one subfolder per
    /// project, one file per note (body verbatim). Notes without a project
    /// are skipped.
    Export {
        /// Destination directory (created if missing)
        dir: PathBuf,
        /// Export just this project (a name or proj/<slug>), flat into <dir> —
        /// the folder itself is the project
        #[arg(long)]
        project: Option<String>,
    },
    /// Import a directory of Markdown files as notes. A folder is a project:
    /// files in a subfolder join that subfolder's project; files at the top
    /// level join a project named after the directory itself. Re-importing
    /// the same directory is a no-op (exact-body duplicates are skipped).
    Import {
        /// Directory to scan for .md files
        dir: PathBuf,
        /// Put every imported note in this project (a name or proj/<slug>)
        /// instead of deriving projects from folder names
        #[arg(long)]
        project: Option<String>,
        /// Assign no project at all — notes keep only the tags already in
        /// their bodies (e.g. importing a Bear/Obsidian dump that isn't one
        /// project)
        #[arg(long, conflicts_with = "project")]
        no_project: bool,
    },
    /// Run the sync daemon (foreground): connect known peers, keep notes converged
    Sync {
        /// Sync round interval in milliseconds
        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,
    },
    /// Show the running daemon's peers and state
    SyncStatus,
    /// Manage trusted sync peers (pairing replaces LAN auto-discovery)
    Pair {
        #[command(subcommand)]
        action: PairAction,
    },
}

#[derive(clap::Args)]
pub struct BulkArgs {
    /// Select notes carrying this exact tag (trashed notes for restore)
    #[arg(long)]
    pub tag: Option<String>,
    /// Select notes in this project (trashed notes for restore)
    #[arg(long)]
    pub project: Option<String>,
    /// Select a note by ID; repeat for multiple notes
    #[arg(long = "id", value_parser = note_ref)]
    pub ids: Vec<String>,
    /// Read note IDs from stdin, one per line
    #[arg(long)]
    pub stdin: bool,
    /// Show what would change without writing
    #[arg(long, conflicts_with = "yes")]
    pub dry_run: bool,
    /// Confirm and apply the operation
    #[arg(long, conflicts_with = "dry_run")]
    pub yes: bool,
    #[command(subcommand)]
    pub action: BulkAction,
}

#[derive(Subcommand)]
pub enum BulkAction {
    /// Add or remove a body-derived hashtag
    Tag {
        #[command(subcommand)]
        action: BulkTagAction,
    },
    /// Reclassify the selected notes
    SetType { note_type: String },
    /// Move the selected notes to trash
    Delete,
    /// Restore the selected notes from trash
    Restore,
}

#[derive(Subcommand)]
pub enum BulkTagAction {
    /// Add a hashtag (without the leading #)
    Add { tag: String },
    /// Remove a hashtag (without the leading #)
    Remove { tag: String },
}

#[derive(Subcommand)]
pub enum PairAction {
    /// Show this device's pairing code and wait for one device to pair
    /// (approve it at the prompt); pairs the running daemon if there is one
    Show {
        /// Auto-approve the first device that connects (no prompt)
        #[arg(long)]
        yes: bool,
    },
    /// Trust the device behind a pasted/scanned code and connect to it now
    Add { ticket: String },
}

#[derive(Subcommand)]
pub enum ProjectAction {
    /// Register the current directory as a project: write the .kiem marker, add an
    /// AGENTS.md pointer, and (for a new project) create a home note
    Add { name: String },
    /// List known projects (derived from proj/* tags with note counts)
    List,
    /// Print the project resolved for the current directory
    Current,
}

#[derive(Subcommand)]
pub enum TodoAction {
    /// Append a todo to a note: kiem todo add <note-id> "<text>"
    Add {
        #[arg(value_parser = note_ref)]
        note_id: String,
        text: String,
    },
    /// Mark one or more todos done by their stable checkbox indices
    Check {
        #[arg(value_parser = note_ref)]
        note_id: String,
        #[arg(value_name = "INDEX", num_args = 1..)]
        indices: Vec<usize>,
    },
    /// Mark one or more todos not done by their stable checkbox indices
    Uncheck {
        #[arg(value_parser = note_ref)]
        note_id: String,
        #[arg(value_name = "INDEX", num_args = 1..)]
        indices: Vec<usize>,
    },
}

#[derive(Subcommand)]
pub enum NoteAction {
    /// Add a note to the current project (tags it proj/<slug>). Body from the
    /// positional arg, --file, or stdin — prefer --file/stdin for markdown with
    /// backticks or $(...), which a shell mangles inside a quoted argument.
    Add {
        /// Note text; the first line becomes the title. Omit to use --file or stdin.
        text: Option<String>,
        /// Read the note body from a file instead of the positional arg (safe for
        /// markdown containing shell metacharacters).
        #[arg(long)]
        file: Option<PathBuf>,
        /// Override the resolved project (a name or proj/<slug>)
        #[arg(long)]
        project: Option<String>,
        /// Kind of note (e.g. plan, brainstorm, review, solution, decision, doc).
        /// Defaults to a plain note.
        #[arg(long = "type")]
        note_type: Option<String>,
    },
    /// Reclassify a note's kind: kiem note set-type <id> <type>
    SetType {
        #[arg(value_parser = note_ref)]
        note_id: String,
        /// New kind (empty resets to the default note)
        note_type: String,
    },
}
