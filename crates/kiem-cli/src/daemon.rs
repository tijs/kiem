//! The sync daemon: a thin CLI wrapper around `kiem_sync::Mesh`.
//!
//! The mesh (identity, discovery, accept/dial loops, per-connection sync)
//! lives in `kiem-sync`, shared with the Swift app's FFI bridge. This module
//! just owns the CLI-specific bits: opening the store, the status-file
//! heartbeat for `kiem sync-status`, and stderr logging.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use kiem_core::store::NoteStore;
use kiem_core::sync::SyncEngine;
use kiem_sync::{EndpointId, Mesh, MeshEvents};
use serde_json::json;

pub struct Options {
    pub data_dir: PathBuf,
    pub interval: Duration,
}

const STATUS_FILE: &str = "sync-status.json";

struct LogEvents;
impl MeshEvents for LogEvents {
    fn on_connected(&self, peer: EndpointId) {
        eprintln!("kiem sync: connected to peer {peer}");
    }
    fn on_disconnected(&self, peer: EndpointId) {
        eprintln!("kiem sync: peer {peer} disconnected");
    }
}

pub fn run(opts: Options) -> Result<()> {
    tokio::runtime::Runtime::new()
        .context("starting async runtime")?
        .block_on(run_async(opts))
}

async fn run_async(opts: Options) -> Result<()> {
    let store = NoteStore::open_dir(&opts.data_dir)
        .with_context(|| format!("opening data directory {}", opts.data_dir.display()))?;
    let state = Arc::new(Mutex::new((store, SyncEngine::new())));

    let mesh = Mesh::start(opts.data_dir.clone(), state, opts.interval, Arc::new(LogEvents))
        .await
        .context("starting sync mesh")?;
    eprintln!("kiem sync: endpoint {} ready", mesh.endpoint_id());

    // Heartbeat: publish status every second, forever.
    loop {
        write_status(&opts.data_dir, &mesh)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn write_status(data_dir: &Path, mesh: &Mesh) -> Result<()> {
    let peers: Vec<_> = mesh
        .connected_ids()
        .into_iter()
        .map(|id| json!({"peer_id": id.to_string()}))
        .collect();
    let status = json!({
        "endpoint_id": mesh.endpoint_id().to_string(),
        "peers": peers,
        "updated_at_epoch_secs": epoch_secs(),
    });
    // Write-then-rename so readers never see a torn file.
    let path = data_dir.join(STATUS_FILE);
    let tmp = data_dir.join(format!("{STATUS_FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(&status)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// `kiem sync-status`: read what the daemon last published.
pub fn print_status(data_dir: &Path, as_json: bool) -> Result<()> {
    let path = data_dir.join(STATUS_FILE);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| "no sync status found — is `kiem sync` running?")?;
    let status: serde_json::Value = serde_json::from_str(&raw)?;

    let age = epoch_secs().saturating_sub(status["updated_at_epoch_secs"].as_u64().unwrap_or(0));
    let stale = age > 5;
    if as_json {
        let mut v = status;
        v["stale"] = json!(stale);
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if stale {
        println!("daemon appears stopped (status is {age}s old)");
    }
    println!("endpoint: {}", status["endpoint_id"].as_str().unwrap_or("?"));
    let peers = status["peers"].as_array().cloned().unwrap_or_default();
    if peers.is_empty() {
        println!("peers: none connected");
    } else {
        println!("peers:");
        for p in peers {
            println!("  {}", p["peer_id"].as_str().unwrap_or("?"));
        }
    }
    Ok(())
}

/// `kiem pair show`: this device's shareable ticket (paste/scan on another
/// device to add it as a known peer).
pub async fn pair_show(data_dir: &Path) -> Result<String> {
    Ok(kiem_sync::pair_ticket(data_dir).await?)
}

/// `kiem pair add <ticket>`: trust the device behind a pasted/scanned ticket.
pub fn pair_add(data_dir: &Path, ticket: &str) -> Result<EndpointId> {
    Ok(kiem_sync::pair_add(data_dir, ticket)?)
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
