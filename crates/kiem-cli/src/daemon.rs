//! The sync daemon: iroh-based P2P sync.
//!
//! Connections are driven by `kiem_sync::run_session`, which speaks the
//! framed protocol from `kiem_core::protocol` over one bidirectional iroh
//! stream per connection. Peers come from the known-peers trust list
//! (`kiem_sync::KnownPeers`) rather than LAN broadcast — iroh's discovery and
//! relay find a peer wherever it actually is. `kiem pair` manages that list.
//!
//! Status for `kiem sync-status` is published as `sync-status.json` in the
//! data dir, rewritten every second by a heartbeat loop.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use iroh::endpoint::Connection;
use kiem_core::store::NoteStore;
use kiem_core::sync::SyncEngine;
use kiem_sync::{Endpoint, EndpointAddr, EndpointId, KnownPeers, SharedState};
use serde_json::json;

pub struct Options {
    pub data_dir: PathBuf,
    pub interval: Duration,
}

const STATUS_FILE: &str = "sync-status.json";
const IDENTITY_FILE: &str = "identity.key";
const PEERS_FILE: &str = "known-peers";
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

struct Daemon {
    endpoint: Endpoint,
    data_dir: PathBuf,
    state: SharedState,
    /// Peers with a live connection right now, for status + dial/accept dedupe.
    connected: Mutex<HashSet<EndpointId>>,
    interval: Duration,
}

pub fn run(opts: Options) -> Result<()> {
    tokio::runtime::Runtime::new()
        .context("starting async runtime")?
        .block_on(run_async(opts))
}

async fn run_async(opts: Options) -> Result<()> {
    let secret_key = kiem_sync::load_or_create(&opts.data_dir.join(IDENTITY_FILE))
        .context("loading device identity")?;
    let store = NoteStore::open_dir(&opts.data_dir)
        .with_context(|| format!("opening data directory {}", opts.data_dir.display()))?;
    let endpoint = kiem_sync::bind(secret_key)
        .await
        .context("binding iroh endpoint")?;

    eprintln!("kiem sync: endpoint {} ready", endpoint.id());

    let daemon = Arc::new(Daemon {
        endpoint,
        data_dir: opts.data_dir.clone(),
        state: Arc::new(Mutex::new((store, SyncEngine::new()))),
        connected: Mutex::new(HashSet::new()),
        interval: opts.interval,
    });

    tokio::spawn(accept_loop(daemon.clone()));

    let known = KnownPeers::load(&daemon.data_dir.join(PEERS_FILE)).context("loading known peers")?;
    for id in known.ids().to_vec() {
        tokio::spawn(dial_loop(daemon.clone(), id));
    }

    // Heartbeat: publish status every second, forever.
    loop {
        write_status(&daemon)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn accept_loop(daemon: Arc<Daemon>) {
    loop {
        match kiem_sync::accept(&daemon.endpoint).await {
            Ok(Some(connection)) => {
                let daemon = daemon.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_connection(daemon, connection, false).await {
                        eprintln!("kiem sync: connection ended: {err:#}");
                    }
                });
            }
            Ok(None) => return, // endpoint closed
            Err(err) => eprintln!("kiem sync: accept error: {err:#}"),
        }
    }
}

/// Endless dial for a known peer — covers reconnection after a restart or a
/// network change, which is the entire point of moving off LAN-only mDNS.
async fn dial_loop(daemon: Arc<Daemon>, id: EndpointId) {
    loop {
        if !daemon.connected.lock().unwrap().contains(&id) {
            match kiem_sync::connect(&daemon.endpoint, EndpointAddr::from(id)).await {
                Ok(connection) => {
                    if let Err(err) = handle_connection(daemon.clone(), connection, true).await {
                        eprintln!("kiem sync: connection to {id} ended: {err:#}");
                    }
                }
                Err(err) => eprintln!("kiem sync: cannot reach {id}: {err}"),
            }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// Runs one session to completion. `dialed` picks which side opens the
/// bidirectional stream (see `kiem_sync::run_session`).
async fn handle_connection(daemon: Arc<Daemon>, connection: Connection, dialed: bool) -> Result<()> {
    let peer = connection.remote_id();
    if !daemon.connected.lock().unwrap().insert(peer) {
        return Ok(()); // already linked to this peer (a dial/accept race)
    }
    eprintln!("kiem sync: connected to peer {peer}");
    let result = kiem_sync::run_session(connection, dialed, daemon.state.clone(), daemon.interval).await;
    daemon.connected.lock().unwrap().remove(&peer);
    eprintln!("kiem sync: peer {peer} disconnected");
    result.map_err(anyhow::Error::from)
}

fn write_status(daemon: &Daemon) -> Result<()> {
    let peers: Vec<_> = daemon
        .connected
        .lock()
        .unwrap()
        .iter()
        .map(|id| json!({"peer_id": id.to_string()}))
        .collect();
    let status = json!({
        "endpoint_id": daemon.endpoint.id().to_string(),
        "peers": peers,
        "updated_at_epoch_secs": epoch_secs(),
    });
    // Write-then-rename so readers never see a torn file.
    let path = daemon.data_dir.join(STATUS_FILE);
    let tmp = daemon.data_dir.join(format!("{STATUS_FILE}.tmp"));
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
    let secret_key = kiem_sync::load_or_create(&data_dir.join(IDENTITY_FILE))
        .context("loading device identity")?;
    let endpoint = kiem_sync::bind(secret_key)
        .await
        .context("binding iroh endpoint")?;
    let ticket = kiem_sync::my_ticket(&endpoint).to_string();
    endpoint.close().await;
    Ok(ticket)
}

/// `kiem pair add <ticket>`: trust the device behind a pasted/scanned ticket.
pub fn pair_add(data_dir: &Path, ticket: &str) -> Result<EndpointId> {
    let addr = kiem_sync::parse_ticket(ticket).context("parsing pairing ticket")?;
    let peers_path = data_dir.join(PEERS_FILE);
    let mut peers = KnownPeers::load(&peers_path).context("loading known peers")?;
    peers.add(&peers_path, addr.id).context("saving known peers")?;
    Ok(addr.id)
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
