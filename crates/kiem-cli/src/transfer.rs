//! Import and export as a directory of Markdown files. The work is
//! `kiem_core::transfer`'s; what lives here is flag shaping and reporting —
//! including translating the core's project error into the flags that fix it,
//! which the core cannot name.

use std::path::{Path, PathBuf};

use anyhow::Result;
use kiem_core::store::NoteStore;
use kiem_core::transfer;
use serde_json::json;

use crate::author;
use crate::output::{display_title, print_json, tag_suffix};
use crate::project;

pub fn export(store: &NoteStore, dir: PathBuf, only: Option<String>, as_json: bool) -> Result<()> {
    match only {
        Some(name) => {
            let tag = project::require_tag(&name)?;
            let written = transfer::export_project(store, &dir, &tag)?;
            if as_json {
                print_json(&json!({"written": written, "project": tag}))?;
            } else {
                println!("Exported {written} notes from {tag} to {}", dir.display());
            }
        }
        None => {
            let (written, skipped) = transfer::export_all(store, &dir)?;
            if as_json {
                print_json(&json!({"written": written, "skipped_without_project": skipped}))?;
            } else {
                println!("Exported {written} notes to {}", dir.display());
                if skipped > 0 {
                    println!("(skipped {skipped} notes without a project — export is per-project; give them a #proj/<slug> tag to include them)");
                }
            }
        }
    }
    Ok(())
}

pub fn import(store: &mut NoteStore, data_dir: &Path, dir: PathBuf, project_override: Option<String>, no_project: bool, as_json: bool) -> Result<()> {
    let tag_override = project_override
        .as_deref()
        .map(project::require_tag)
        .transpose()?;
    let source = match (&tag_override, no_project) {
        (_, true) => transfer::ProjectSource::None,
        (Some(tag), _) => transfer::ProjectSource::Tag(tag),
        (None, false) => transfer::ProjectSource::Folders,
    };
    let (created, skipped) =
        transfer::import(store, &dir, &author(data_dir)?, source)
            // Core can't name CLI flags; "explicitly" means --project here.
            .map_err(|e| match e {
                transfer::TransferError::NoProject { .. } => {
                    anyhow::anyhow!("{e} (--project <name>, or --no-project)")
                }
                other => other.into(),
            })?;
    if as_json {
        let created: Vec<_> = created
            .iter()
            .map(|(file, meta)| json!({"file": file.display().to_string(), "note": meta}))
            .collect();
        print_json(&json!({"created": created, "skipped_duplicates": skipped}))?;
    } else {
        for (_, meta) in &created {
            println!("{}  {}{}", meta.id, display_title(meta), tag_suffix(meta));
        }
        println!(
            "Imported {} notes from {}{}",
            created.len(),
            dir.display(),
            if skipped > 0 {
                format!(" ({skipped} already present, skipped)")
            } else {
                String::new()
            }
        );
    }
    Ok(())
}
