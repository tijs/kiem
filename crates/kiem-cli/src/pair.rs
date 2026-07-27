//! `kiem pair` — the CLI pairing flows. If the sync daemon is running, its
//! control socket does the work (pairing must run inside the process that owns
//! the identity's accept loop); otherwise a transient mesh is bound for just
//! this pairing.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use kiem_core::store::NoteStore;
use kiem_sync::{EndpointId, KnownPeers, Mesh, MeshEvents, PEERS_FILE};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::net::unix::OwnedReadHalf;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::control::{self, send_line, Request, Response};

/// How long an armed pairing window stays open (matches the app's Sync pane).
const WINDOW: Duration = Duration::from_secs(120);
/// How long `pair add` waits for the first connect before settling for
/// "recorded — it will link up when both devices are online".
const ADD_CONNECT_WAIT: Duration = Duration::from_secs(60);

/// `kiem pair show [--yes]`: arm the window, print this device's code, wait
/// for one device to pair (approving it at the prompt), report it, exit.
pub async fn show(data_dir: &Path, yes: bool, as_json: bool) -> Result<()> {
    match UnixStream::connect(data_dir.join(control::SOCKET_FILE)).await {
        Ok(stream) => show_via_daemon(stream, yes, as_json).await,
        Err(_) => show_transient(data_dir, yes, as_json).await,
    }
}

/// `kiem pair add <ticket>`: trust the ticket's device, dial it now, report
/// whether it connected.
pub async fn add(data_dir: &Path, ticket: &str, as_json: bool) -> Result<()> {
    match UnixStream::connect(data_dir.join(control::SOCKET_FILE)).await {
        Ok(stream) => add_via_daemon(stream, ticket, as_json).await,
        Err(_) => add_transient(data_dir, ticket, as_json).await,
    }
}

/// `kiem pair list`: the trust list, with remembered names. Reads the
/// known-peers file, so it works with or without a running daemon — and it is
/// where the ids for `kiem pair forget` come from (`sync-status` only shows
/// peers that are currently connected, which a device you no longer have
/// never is).
pub fn list(data_dir: &Path, as_json: bool) -> Result<()> {
    let known = KnownPeers::load(&data_dir.join(PEERS_FILE))?;
    let peers: Vec<(EndpointId, Option<String>)> = known
        .ids()
        .into_iter()
        .map(|id| {
            let name = kiem_sync::peer_name(data_dir, &id);
            (id, name)
        })
        .collect();
    if as_json {
        let rows: Vec<_> = peers
            .iter()
            .map(|(id, name)| json!({ "peer_id": id.to_string(), "name": name }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if peers.is_empty() {
        println!("no paired devices");
    } else {
        for (id, name) in &peers {
            match name {
                Some(name) => println!("{id}  {name}"),
                None => println!("{id}"),
            }
        }
    }
    Ok(())
}

/// `kiem pair forget <peer-id>`: unpair a device. Through the daemon when one
/// is running — it owns the live connection and the in-memory sync state, and
/// only it can close and drop them — otherwise straight at the files.
pub async fn forget(data_dir: &Path, peer_id: &str, as_json: bool) -> Result<()> {
    let peer: EndpointId = peer_id
        .trim()
        .parse()
        .with_context(|| format!("{peer_id} is not a peer id (see `kiem pair list`)"))?;
    let known = match UnixStream::connect(data_dir.join(control::SOCKET_FILE)).await {
        Ok(stream) => forget_via_daemon(stream, &peer).await?,
        Err(_) => {
            let store = NoteStore::open_dir(data_dir)
                .with_context(|| format!("opening data directory {}", data_dir.display()))?;
            kiem_sync::forget(data_dir, &kiem_sync::shared_state(store), &peer)?
        }
    };
    print_forgotten(&peer.to_string(), known, as_json);
    Ok(())
}

// MARK: via the daemon's control socket

async fn show_via_daemon(stream: UnixStream, yes: bool, as_json: bool) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    send_line(
        &mut write,
        &Request::Show {
            window_secs: WINDOW.as_secs(),
        },
    )
    .await?;

    // Slack past the window: the daemon's ticket wait runs inside it.
    let deadline = tokio::time::Instant::now() + WINDOW + Duration::from_secs(15);
    loop {
        match read_response(&mut lines, deadline).await? {
            None => bail!("pairing window expired — no device paired"),
            Some(Response::Ticket(ticket)) => print_ticket(&ticket, as_json),
            Some(Response::Approve(peer)) => {
                let allow = yes || prompt_allow(&peer).await?;
                send_line(&mut write, &Request::Allow(allow)).await?;
            }
            Some(Response::Paired(peer)) => {
                print_paired(&peer, as_json);
                return Ok(());
            }
            Some(Response::Error(message)) => bail!("daemon: {message}"),
            // Not part of a show session.
            Some(Response::Added(_) | Response::Forgotten { .. }) => {}
        }
    }
}

async fn add_via_daemon(stream: UnixStream, ticket: &str, as_json: bool) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    send_line(
        &mut write,
        &Request::Add {
            ticket: ticket.to_owned(),
        },
    )
    .await?;

    let deadline = tokio::time::Instant::now() + ADD_CONNECT_WAIT;
    let mut added = None;
    loop {
        match read_response(&mut lines, deadline).await? {
            Some(Response::Added(peer)) => {
                if !as_json {
                    eprintln!("added peer {peer}; connecting…");
                }
                added = Some(peer);
            }
            Some(Response::Paired(peer)) => {
                print_paired(&peer, as_json);
                return Ok(());
            }
            Some(Response::Error(message)) => bail!("daemon: {message}"),
            Some(_) => {}
            None => {
                // No connect within the wait — the daemon keeps dialing it.
                let peer = added.context("the daemon never acknowledged the add")?;
                print_added_pending(&peer, as_json);
                return Ok(());
            }
        }
    }
}

async fn forget_via_daemon(stream: UnixStream, peer: &EndpointId) -> Result<bool> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    send_line(
        &mut write,
        &Request::Forget {
            peer_id: peer.to_string(),
        },
    )
    .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    match read_response(&mut lines, deadline).await? {
        Some(Response::Forgotten { known }) => Ok(known),
        Some(Response::Error(err)) => bail!("{err}"),
        _ => bail!("the daemon did not acknowledge the unpair"),
    }
}

/// One control response, or `None` once the deadline passes.
async fn read_response(
    lines: &mut Lines<BufReader<OwnedReadHalf>>,
    deadline: tokio::time::Instant,
) -> Result<Option<Response>> {
    let Ok(line) = tokio::time::timeout_at(deadline, lines.next_line()).await else {
        return Ok(None);
    };
    let line = line?.context("the daemon closed the control connection")?;
    Ok(Some(
        serde_json::from_str(&line).context("bad control response")?,
    ))
}

async fn prompt_allow(peer: &str) -> Result<bool> {
    let peer = peer.to_owned();
    tokio::task::spawn_blocking(move || ask(&peer))
        .await
        .context("reading the approval answer")
}

// MARK: transient mesh (no daemon running)

async fn show_transient(data_dir: &Path, yes: bool, as_json: bool) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let events = Arc::new(TerminalEvents::new(yes, true, tx));
    let mesh = start_mesh(data_dir, events.clone()).await?;
    mesh.arm_pairing(WINDOW);
    let ticket = mesh.ticket_online().await;
    print_ticket(&ticket, as_json);

    let deadline = tokio::time::Instant::now() + WINDOW;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Err(_) => bail!("pairing window expired — no device paired"),
            Ok(None) => bail!("sync mesh stopped unexpectedly"),
            // Known peers may reconnect mid-window; only the device approved
            // at the prompt counts as the pairing.
            Ok(Some(peer)) if events.approved(&peer) => {
                settle(data_dir, &peer).await;
                print_paired(&peer.to_string(), as_json);
                return Ok(());
            }
            Ok(Some(_)) => {}
        }
    }
}

async fn add_transient(data_dir: &Path, ticket: &str, as_json: bool) -> Result<()> {
    // Record before dialing: even if the connect fails now, every future mesh
    // start keeps dialing this peer.
    let addr = kiem_sync::pair_add(data_dir, ticket)?;
    let id = addr.id;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let events = Arc::new(TerminalEvents::new(false, false, tx));
    let mesh = start_mesh(data_dir, events).await?;
    mesh.pair_dial(addr);
    if !as_json {
        eprintln!("added peer {id}; connecting…");
    }

    let deadline = tokio::time::Instant::now() + ADD_CONNECT_WAIT;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Err(_) => {
                print_added_pending(&id.to_string(), as_json);
                return Ok(());
            }
            Ok(None) => bail!("sync mesh stopped unexpectedly"),
            Ok(Some(peer)) if peer == id => {
                settle(data_dir, &peer).await;
                print_paired(&peer.to_string(), as_json);
                return Ok(());
            }
            Ok(Some(_)) => {}
        }
    }
}

async fn start_mesh(data_dir: &Path, events: Arc<TerminalEvents>) -> Result<Arc<Mesh>> {
    let store = NoteStore::open_dir(data_dir)
        .with_context(|| format!("opening data directory {}", data_dir.display()))?;
    let state = kiem_sync::shared_state(store);
    Ok(Mesh::start(data_dir.to_owned(), state, Duration::from_secs(1), events).await?)
}

/// The connect event fires before the session's reciprocal-trust prelude and
/// first sync round run — a transient process must not tear its endpoint down
/// before they land. Wait for our own record of the peer, then a beat more.
async fn settle(data_dir: &Path, peer: &EndpointId) {
    let peers_path = data_dir.join(PEERS_FILE);
    for _ in 0..100 {
        let recorded = KnownPeers::load(&peers_path)
            .map(|known| known.contains(peer))
            .unwrap_or(false);
        if recorded {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // ponytail: fixed grace for the peer's own record + first sync round; a
    // handshake-complete event on MeshEvents is the precise fix if this flakes.
    tokio::time::sleep(Duration::from_secs(2)).await;
}

/// `MeshEvents` for a transient pairing mesh: prompts at the terminal (the
/// gate already calls from a blocking thread), reports connects on a channel,
/// and deduplicates dial-error noise (a known-but-offline peer would otherwise
/// repeat the same error every reconnect attempt of the window).
struct TerminalEvents {
    /// `--yes`: auto-approve the first device, no prompt.
    yes: bool,
    /// `pair add` never approves strangers; only `pair show` opens that door.
    can_approve: bool,
    connects: mpsc::UnboundedSender<EndpointId>,
    approved: Mutex<Option<EndpointId>>,
    seen_errors: Mutex<HashSet<String>>,
}

impl TerminalEvents {
    fn new(yes: bool, can_approve: bool, connects: mpsc::UnboundedSender<EndpointId>) -> Self {
        Self {
            yes,
            can_approve,
            connects,
            approved: Mutex::new(None),
            seen_errors: Mutex::new(HashSet::new()),
        }
    }

    fn approved(&self, peer: &EndpointId) -> bool {
        *self.approved.lock().unwrap() == Some(*peer)
    }
}

impl MeshEvents for TerminalEvents {
    fn on_connected(&self, peer: EndpointId) {
        let _ = self.connects.send(peer);
    }

    fn on_error(&self, context: &str, error: &str) {
        if self
            .seen_errors
            .lock()
            .unwrap()
            .insert(format!("{context}: {error}"))
        {
            eprintln!("kiem pair: {context}: {error}");
        }
    }

    fn approve_pairing(&self, peer: EndpointId) -> bool {
        if !self.can_approve {
            return false;
        }
        let allow = self.yes || ask(&peer.to_string());
        if allow {
            *self.approved.lock().unwrap() = Some(peer);
        }
        allow
    }
}

/// y/N prompt on the terminal. EOF (a non-interactive stdin) denies — the
/// safe default when an agent pipes `pair show` without `--yes`.
fn ask(peer: &str) -> bool {
    eprint!("Pair with device {peer}? [y/N] ");
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).ok();
    matches!(answer.trim(), "y" | "Y" | "yes")
}

// MARK: output

fn print_ticket(ticket: &str, as_json: bool) {
    if as_json {
        println!("{}", json!({ "ticket": ticket }));
    } else {
        println!("{ticket}");
        eprintln!();
        eprintln!(
            "On the other device: Settings → Sync → Add a device (or `kiem pair add <code>`)."
        );
        eprintln!(
            "Waiting for a device to pair ({}s window, Ctrl-C to cancel)…",
            WINDOW.as_secs()
        );
    }
}

fn print_paired(peer: &str, as_json: bool) {
    if as_json {
        println!("{}", json!({ "paired": peer }));
    } else {
        println!("paired with {peer}");
    }
}

fn print_forgotten(peer: &str, known: bool, as_json: bool) {
    if as_json {
        println!("{}", json!({ "forgot": peer, "was_paired": known }));
    } else if known {
        println!("unpaired {peer} — it can no longer sync with this device");
    } else {
        println!("{peer} was not a paired device");
    }
}

fn print_added_pending(peer: &str, as_json: bool) {
    if as_json {
        println!("{}", json!({ "added": peer, "connected": false }));
    } else {
        println!(
            "added peer {peer} — not reachable yet; it will link up when both devices are online"
        );
    }
}
