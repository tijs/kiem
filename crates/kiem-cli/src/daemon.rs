//! The sync daemon: peer discovery + Automerge sync over TCP.
//!
//! Connections carry the framed protocol from `kiem_core::protocol`. The
//! first frame each way is a control frame holding the sender's peer id;
//! after that, data frames carry per-document sync messages. Each connection
//! runs a reader thread (apply incoming, respond immediately) and a ticker
//! thread (periodic sync round, which also picks up local edits made by
//! other `kiem` processes against the same data dir).
//!
//! Discovery is mDNS (`_kiem._tcp.local.`) via `mdns-sd`; `--connect` gives
//! direct addresses (tests, and later cross-network). To avoid duplicate
//! links when two peers discover each other simultaneously, only the side
//! with the lexicographically smaller peer id dials.
//!
//! Status for `kiem sync-status` is published as `sync-status.json` in the
//! data dir, rewritten every second by a heartbeat loop.

use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use kiem_core::protocol::{read_frame, write_frame, Frame};
use kiem_core::store::NoteStore;
use kiem_core::sync::SyncEngine;
use serde_json::json;

pub struct Options {
    pub data_dir: PathBuf,
    pub listen: String,
    pub connect: Vec<String>,
    pub mdns: bool,
    pub interval: Duration,
}

const SERVICE_TYPE: &str = "_kiem._tcp.local.";
const STATUS_FILE: &str = "sync-status.json";
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

struct Daemon {
    peer_id: String,
    data_dir: PathBuf,
    listen_port: Mutex<u16>,
    state: Mutex<(NoteStore, SyncEngine)>,
    /// peer id → remote address, for status + duplicate-connection dedupe.
    peers: Mutex<HashMap<String, String>>,
    interval: Duration,
}

pub fn run(opts: Options) -> Result<()> {
    let peer_id = load_or_create_peer_id(&opts.data_dir)?;
    let store = NoteStore::open_dir(&opts.data_dir)
        .with_context(|| format!("opening data directory {}", opts.data_dir.display()))?;
    let listener =
        TcpListener::bind(&opts.listen).with_context(|| format!("binding {}", opts.listen))?;
    let port = listener.local_addr()?.port();

    let daemon = Arc::new(Daemon {
        peer_id: peer_id.clone(),
        data_dir: opts.data_dir.clone(),
        listen_port: Mutex::new(port),
        state: Mutex::new((store, SyncEngine::new())),
        peers: Mutex::new(HashMap::new()),
        interval: opts.interval,
    });

    eprintln!("kiem sync: peer {peer_id} listening on port {port}");

    {
        let daemon = daemon.clone();
        std::thread::spawn(move || accept_loop(daemon, listener));
    }
    for addr in &opts.connect {
        let daemon = daemon.clone();
        let addr = addr.clone();
        std::thread::spawn(move || dial_loop(daemon, addr));
    }
    // Keep the mDNS daemon alive for the process lifetime by holding it here.
    let _mdns = if opts.mdns {
        Some(start_mdns(daemon.clone(), port)?)
    } else {
        None
    };

    // Heartbeat: publish status every second, forever.
    loop {
        write_status(&daemon)?;
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn accept_loop(daemon: Arc<Daemon>, listener: TcpListener) {
    for stream in listener.incoming().flatten() {
        let daemon = daemon.clone();
        std::thread::spawn(move || {
            if let Err(err) = handle_connection(daemon, stream) {
                eprintln!("kiem sync: connection ended: {err:#}");
            }
        });
    }
}

/// Endless dial for explicit `--connect` addresses (fixed setups; covers
/// reconnection after the remote restarts).
fn dial_loop(daemon: Arc<Daemon>, addr: String) {
    loop {
        match TcpStream::connect(&addr) {
            Ok(stream) => {
                if let Err(err) = handle_connection(daemon.clone(), stream) {
                    eprintln!("kiem sync: connection to {addr} ended: {err:#}");
                }
            }
            Err(err) => eprintln!("kiem sync: cannot reach {addr}: {err}"),
        }
        std::thread::sleep(RECONNECT_DELAY);
    }
}

/// Bounded dial for an mDNS-discovered peer: stop as soon as the peer is
/// connected (possibly via another address or its own dial), give up after
/// a few rounds — the next mDNS resolution retriggers discovery anyway.
fn dial_discovered(daemon: Arc<Daemon>, instance: String, targets: Vec<std::net::SocketAddr>) {
    for _ in 0..5 {
        if daemon.peers.lock().unwrap().contains_key(&instance) {
            return;
        }
        for target in &targets {
            if let Ok(stream) = TcpStream::connect(target) {
                if let Err(err) = handle_connection(daemon.clone(), stream) {
                    eprintln!("kiem sync: connection to {instance} ended: {err:#}");
                }
                return;
            }
        }
        std::thread::sleep(RECONNECT_DELAY);
    }
}

/// Hello exchange, then reader loop + sync ticker until the peer goes away.
fn handle_connection(daemon: Arc<Daemon>, stream: TcpStream) -> Result<()> {
    let remote = stream.peer_addr()?.to_string();
    let mut reader = BufReader::new(stream.try_clone()?);
    let writer = Arc::new(Mutex::new(BufWriter::new(stream)));

    write_locked(&writer, &Frame::control(daemon.peer_id.as_bytes().to_vec()))?;
    let hello = read_frame(&mut reader)?;
    anyhow::ensure!(hello.is_control(), "peer did not start with a hello frame");
    let peer = String::from_utf8(hello.payload).context("peer id is not UTF-8")?;

    if peer == daemon.peer_id {
        return Ok(()); // mDNS found ourselves
    }
    {
        let mut peers = daemon.peers.lock().unwrap();
        if peers.contains_key(&peer) {
            return Ok(()); // already linked to this peer
        }
        peers.insert(peer.clone(), remote);
    }
    eprintln!("kiem sync: connected to peer {peer}");

    // Ticker: periodic full sync round (also picks up local edits).
    let ticker = {
        let daemon = daemon.clone();
        let writer = writer.clone();
        let peer = peer.clone();
        std::thread::spawn(move || {
            while sync_round(&daemon, &peer, &writer).is_ok() {
                std::thread::sleep(daemon.interval);
            }
        })
    };

    // Reader: apply incoming messages, answer immediately per document.
    let result = (|| -> Result<()> {
        loop {
            let frame = read_frame(&mut reader)?;
            if frame.is_control() {
                continue;
            }
            let reply = {
                let (store, engine) = &mut *daemon.state.lock().unwrap();
                engine.receive_message(store, &peer, &frame.doc_id, &frame.payload)?;
                engine.generate_message(store, &peer, &frame.doc_id)?
            };
            if let Some(payload) = reply {
                write_locked(&writer, &Frame { doc_id: frame.doc_id, payload })?;
            }
        }
    })();

    daemon.peers.lock().unwrap().remove(&peer);
    daemon.state.lock().unwrap().1.forget_peer(&peer);
    drop(writer); // unblock the ticker's next write
    let _ = ticker.join();
    eprintln!("kiem sync: peer {peer} disconnected");
    result
}

/// One sync round: offer every known document to the peer.
fn sync_round(
    daemon: &Daemon,
    peer: &str,
    writer: &Arc<Mutex<BufWriter<TcpStream>>>,
) -> Result<()> {
    let frames = {
        let (store, engine) = &mut *daemon.state.lock().unwrap();
        let mut frames = Vec::new();
        for doc_id in engine.doc_ids(store)? {
            if let Some(payload) = engine.generate_message(store, peer, &doc_id)? {
                frames.push(Frame { doc_id, payload });
            }
        }
        frames
    };
    for frame in &frames {
        write_locked(writer, frame)?;
    }
    Ok(())
}

fn write_locked(
    writer: &Arc<Mutex<BufWriter<TcpStream>>>,
    frame: &Frame,
) -> Result<()> {
    let mut w = writer.lock().unwrap();
    write_frame(&mut *w, frame)?;
    w.flush()?;
    Ok(())
}

fn start_mdns(daemon: Arc<Daemon>, port: u16) -> Result<mdns_sd::ServiceDaemon> {
    use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

    let mdns = ServiceDaemon::new().context("starting mDNS daemon")?;
    let hostname = format!("{}.local.", daemon.peer_id);
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        &daemon.peer_id,
        &hostname,
        (),
        port,
        None,
    )?
    .enable_addr_auto();
    mdns.register(info).context("registering mDNS service")?;

    let receiver = mdns.browse(SERVICE_TYPE).context("browsing mDNS")?;
    let dialer = daemon.clone();
    std::thread::spawn(move || {
        while let Ok(event) = receiver.recv() {
            if let ServiceEvent::ServiceResolved(info) = event {
                let instance = info
                    .get_fullname()
                    .trim_end_matches(&format!(".{SERVICE_TYPE}"))
                    .to_string();
                // Dedupe: lower peer id dials, and never dial ourselves or
                // an already-connected peer.
                if instance == dialer.peer_id
                    || dialer.peer_id > instance
                    || dialer.peers.lock().unwrap().contains_key(&instance)
                {
                    continue;
                }
                // Prefer IPv4 (the listener binds an IPv4 any-address);
                // SocketAddr formatting brackets IPv6 correctly.
                let port = info.get_port();
                let mut targets: Vec<std::net::SocketAddr> = info
                    .get_addresses()
                    .iter()
                    .map(|ip| std::net::SocketAddr::new(ip.to_ip_addr(), port))
                    .collect();
                targets.sort_by_key(|a| !a.is_ipv4());
                let daemon = dialer.clone();
                std::thread::spawn(move || dial_discovered(daemon, instance, targets));
            }
        }
    });
    Ok(mdns)
}

fn write_status(daemon: &Daemon) -> Result<()> {
    let peers: Vec<_> = daemon
        .peers
        .lock()
        .unwrap()
        .iter()
        .map(|(id, addr)| json!({"peer_id": id, "address": addr}))
        .collect();
    let status = json!({
        "peer_id": daemon.peer_id,
        "listen_port": *daemon.listen_port.lock().unwrap(),
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
    println!("peer:  {}", status["peer_id"].as_str().unwrap_or("?"));
    println!("port:  {}", status["listen_port"]);
    let peers = status["peers"].as_array().cloned().unwrap_or_default();
    if peers.is_empty() {
        println!("peers: none connected");
    } else {
        println!("peers:");
        for p in peers {
            println!("  {} ({})", p["peer_id"].as_str().unwrap_or("?"), p["address"].as_str().unwrap_or("?"));
        }
    }
    Ok(())
}

fn load_or_create_peer_id(data_dir: &Path) -> Result<String> {
    std::fs::create_dir_all(data_dir)?;
    let path = data_dir.join("peer-id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim().to_owned();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    std::fs::write(&path, &id)?;
    Ok(id)
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
