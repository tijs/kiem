//! The sync daemon's control socket. Pairing must run inside the process that
//! owns the identity's accept loop — a second endpoint online with the same
//! key corrupts discovery — so while the daemon runs, `kiem pair` drives
//! pairing through this socket instead of binding its own mesh.
//!
//! Wire format: line-delimited JSON over a Unix socket at
//! `<data-dir>/control.sock`, mode 0600. The socket is a trust boundary:
//! whoever can write it can approve a pairing.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use kiem_sync::{EndpointId, Mesh, MeshEvents};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

pub const SOCKET_FILE: &str = "control.sock";

/// Client → daemon. One `Show` or `Add` opens the session; `Allow` answers an
/// `Approve` push.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    /// Arm the pairing window and wait for one device to pair.
    Show { window_secs: u64 },
    /// Trust a pasted ticket and dial it now.
    Add { ticket: String },
    /// Unpair a device: drop its trust, state and live connection.
    Forget { peer_id: String },
    /// The user's answer to an `Approve` push.
    Allow(bool),
}

/// Daemon → client.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    /// This device's pairing ticket; the window is armed.
    Ticket(String),
    /// An unknown device wants to pair — reply with `Allow`.
    Approve(String),
    /// The device is trusted and connected; the session is over.
    Paired(String),
    /// `Add` recorded the peer (dialing continues in the background).
    Added(String),
    /// `Forget` is done; `known` is false if the peer wasn't paired.
    Forgotten { known: bool },
    Error(String),
}

pub(crate) async fn send_line<T: Serialize>(write: &mut OwnedWriteHalf, message: &T) -> Result<()> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    write.write_all(line.as_bytes()).await?;
    Ok(())
}

/// What the mesh's gate/connect callbacks hand to the attached client session.
pub enum PairEvent {
    /// The trust gate wants an answer for an unknown peer.
    Approve(EndpointId, oneshot::Sender<bool>),
    Connected(EndpointId),
}

/// The daemon's `MeshEvents`: logs to stderr, and while a `kiem pair` client
/// is attached, relays approval prompts and connects to it. With no client
/// attached, unknown peers stay denied (the default-deny gate stands).
#[derive(Default)]
pub struct ControlEvents {
    client: Mutex<Option<mpsc::UnboundedSender<PairEvent>>>,
}

impl ControlEvents {
    fn attach(&self) -> mpsc::UnboundedReceiver<PairEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.client.lock().unwrap() = Some(tx);
        rx
    }

    fn detach(&self) {
        *self.client.lock().unwrap() = None;
    }

    fn send(&self, event: PairEvent) -> bool {
        match &*self.client.lock().unwrap() {
            Some(tx) => tx.send(event).is_ok(),
            None => false,
        }
    }
}

impl MeshEvents for ControlEvents {
    fn on_connected(&self, peer: EndpointId) {
        eprintln!("kiem sync: connected to peer {peer}");
        self.send(PairEvent::Connected(peer));
    }

    fn on_disconnected(&self, peer: EndpointId) {
        eprintln!("kiem sync: peer {peer} disconnected");
    }

    fn on_error(&self, context: &str, error: &str) {
        eprintln!("kiem sync: {context}: {error}");
    }

    /// Runs on a blocking thread (see `Mesh`); blocks on the attached client's
    /// answer. No client, or the client hangs up mid-prompt: deny.
    fn approve_pairing(&self, peer: EndpointId) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        if !self.send(PairEvent::Approve(peer, reply_tx)) {
            return false;
        }
        reply_rx.blocking_recv().unwrap_or(false)
    }
}

/// A live daemon already serves the socket — the one bind failure that must
/// stop a starting daemon (two meshes on one identity corrupt discovery),
/// where any other failure just degrades `kiem pair`.
#[derive(thiserror::Error, Debug)]
#[error("another kiem sync daemon is already running for {data_dir}")]
pub struct AlreadyRunning {
    data_dir: String,
}

/// Binds the control socket. Doubles as the daemon's single-instance lock:
/// refuses to start when another daemon already serves a live socket here.
pub async fn bind(data_dir: &Path) -> Result<UnixListener> {
    let path = data_dir.join(SOCKET_FILE);
    if UnixStream::connect(&path).await.is_ok() {
        return Err(AlreadyRunning {
            data_dir: data_dir.display().to_string(),
        }
        .into());
    }
    let _ = std::fs::remove_file(&path); // stale socket from a crashed daemon
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("binding control socket {}", path.display()))?;
    // Trust boundary: only this user may arm/approve pairing.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting control socket {}", path.display()))?;
    Ok(listener)
}

/// Serves `kiem pair` clients forever, one session at a time (pairing is a
/// rare, human-paced action). Whatever way a session ends, the client is
/// detached and the pairing window disarmed — never left open unattended.
pub async fn serve(
    listener: UnixListener,
    mesh: Arc<Mesh>,
    events: Arc<ControlEvents>,
    data_dir: PathBuf,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        if let Err(err) = handle_client(stream, &mesh, &events, &data_dir).await {
            eprintln!("kiem sync: control: {err:#}");
        }
        events.detach();
        mesh.arm_pairing(Duration::ZERO);
    }
}

async fn handle_client(
    stream: UnixStream,
    mesh: &Arc<Mesh>,
    events: &ControlEvents,
    data_dir: &Path,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let Some(first) = lines.next_line().await? else {
        return Ok(());
    };
    // Attach before arming/dialing so no event can slip past the session.
    let mut rx = events.attach();

    let target = match serde_json::from_str(&first) {
        Ok(Request::Show { window_secs }) => {
            mesh.arm_pairing(Duration::from_secs(window_secs));
            let ticket = mesh.ticket_online().await;
            send_line(&mut write, &Response::Ticket(ticket)).await?;
            None // the paired device is whoever gets approved
        }
        Ok(Request::Add { ticket }) => match kiem_sync::pair_add(data_dir, &ticket) {
            Ok(addr) => {
                let id = addr.id;
                mesh.pair_dial(addr.clone()); // first contact, ignores id ordering
                mesh.dial(addr); // steady-state reconnect loop
                send_line(&mut write, &Response::Added(id.to_string())).await?;
                Some(id)
            }
            Err(err) => {
                send_line(&mut write, &Response::Error(err.to_string())).await?;
                return Ok(());
            }
        },
        // One-shot, unlike Show/Add: there is no pairing to wait for.
        Ok(Request::Forget { peer_id }) => {
            let response = match peer_id.parse() {
                Ok(peer) => match mesh.forget_peer(&peer) {
                    Ok(known) => Response::Forgotten { known },
                    Err(err) => Response::Error(err.to_string()),
                },
                Err(_) => Response::Error(format!("not a peer id: {peer_id}")),
            };
            send_line(&mut write, &response).await?;
            return Ok(());
        }
        Ok(Request::Allow(_)) | Err(_) => {
            send_line(
                &mut write,
                &Response::Error("expected a show or add request".into()),
            )
            .await?;
            return Ok(());
        }
    };
    wait_for_pairing(&mut lines, &mut write, &mut rx, target).await
}

/// The session's main loop: relay approval prompts to the client, report the
/// pairing connect, end when the client hangs up. `target` is the peer whose
/// connect completes the session — fixed for `Add`, set on approval for `Show`.
async fn wait_for_pairing(
    lines: &mut Lines<BufReader<OwnedReadHalf>>,
    write: &mut OwnedWriteHalf,
    rx: &mut mpsc::UnboundedReceiver<PairEvent>,
    mut target: Option<EndpointId>,
) -> Result<()> {
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Some(PairEvent::Approve(peer, reply)) => {
                    if target.is_some() {
                        // An `Add` session never approves strangers on the side.
                        let _ = reply.send(false);
                        continue;
                    }
                    send_line(write, &Response::Approve(peer.to_string())).await?;
                    let allow = matches!(
                        lines.next_line().await?.as_deref().map(serde_json::from_str),
                        Some(Ok(Request::Allow(true)))
                    );
                    let _ = reply.send(allow);
                    if allow {
                        target = Some(peer);
                    }
                }
                Some(PairEvent::Connected(peer)) => {
                    // Known peers reconnect all the time; only this session's
                    // added/approved device counts as the pairing.
                    if target == Some(peer) {
                        send_line(write, &Response::Paired(peer.to_string())).await?;
                        return Ok(());
                    }
                }
                None => return Ok(()),
            },
            // The only unsolicited thing a client sends is EOF (Ctrl-C /
            // hang-up), which ends the session.
            line = lines.next_line() => {
                if line?.is_none() {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiem_core::store::NoteStore;

    async fn start_daemon_side(dir: &Path) -> (Arc<Mesh>, Arc<ControlEvents>) {
        let store = NoteStore::open_dir(dir).unwrap();
        let state = kiem_sync::shared_state(store);
        let events = Arc::new(ControlEvents::default());
        let mesh = Mesh::start(
            dir.to_owned(),
            state,
            Duration::from_millis(200),
            events.clone(),
        )
        .await
        .unwrap();
        let listener = bind(dir).await.unwrap();
        tokio::spawn(serve(
            listener,
            mesh.clone(),
            events.clone(),
            dir.to_owned(),
        ));
        (mesh, events)
    }

    async fn connect(dir: &Path) -> (Lines<BufReader<OwnedReadHalf>>, OwnedWriteHalf) {
        let stream = UnixStream::connect(dir.join(SOCKET_FILE)).await.unwrap();
        let (read, write) = stream.into_split();
        (BufReader::new(read).lines(), write)
    }

    /// Generous: `ticket_online` may wait up to 10s for relay registration.
    async fn read_response(lines: &mut Lines<BufReader<OwnedReadHalf>>) -> Response {
        let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await
            .expect("timed out waiting for a control response")
            .unwrap()
            .expect("daemon closed the control connection");
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn show_session_arms_relays_approval_and_reports_the_pairing() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, events) = start_daemon_side(dir.path()).await;

        let (mut lines, mut write) = connect(dir.path()).await;
        send_line(&mut write, &Request::Show { window_secs: 60 })
            .await
            .unwrap();
        let Response::Ticket(_) = read_response(&mut lines).await else {
            panic!("expected a ticket first");
        };
        assert!(
            mesh.pairing_window_remaining().is_some(),
            "show must arm the window"
        );

        // A stranger knocks: the gate's blocking approve call must surface at
        // the client…
        let stranger = iroh::SecretKey::generate().public();
        let gate = {
            let events = events.clone();
            tokio::task::spawn_blocking(move || events.approve_pairing(stranger))
        };
        let Response::Approve(peer) = read_response(&mut lines).await else {
            panic!("expected an approve push");
        };
        assert_eq!(peer, stranger.to_string());

        // …and the client's allow resolves it.
        send_line(&mut write, &Request::Allow(true)).await.unwrap();
        assert!(gate.await.unwrap(), "allow=true must admit the peer");

        // The connect then completes the session.
        events.on_connected(stranger);
        let Response::Paired(peer) = read_response(&mut lines).await else {
            panic!("expected paired");
        };
        assert_eq!(peer, stranger.to_string());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_client_hangup_disarms_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let (mesh, _events) = start_daemon_side(dir.path()).await;

        let (mut lines, mut write) = connect(dir.path()).await;
        send_line(&mut write, &Request::Show { window_secs: 60 })
            .await
            .unwrap();
        let Response::Ticket(_) = read_response(&mut lines).await else {
            panic!("expected a ticket first");
        };
        assert!(mesh.pairing_window_remaining().is_some());

        drop(lines);
        drop(write);
        for _ in 0..50 {
            if mesh.pairing_window_remaining().is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("window still armed after the client hung up");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn add_records_the_peer_and_acknowledges() {
        let other = tempfile::tempdir().unwrap();
        let ticket = kiem_sync::pair_ticket(other.path()).await.unwrap();

        let dir = tempfile::tempdir().unwrap();
        let (_mesh, _events) = start_daemon_side(dir.path()).await;

        let (mut lines, mut write) = connect(dir.path()).await;
        send_line(&mut write, &Request::Add { ticket })
            .await
            .unwrap();
        let Response::Added(id) = read_response(&mut lines).await else {
            panic!("expected an added ack");
        };

        let known = kiem_sync::KnownPeers::load(&dir.path().join(kiem_sync::PEERS_FILE)).unwrap();
        assert!(
            known.ids().iter().any(|k| k.to_string() == id),
            "added peer must land in the known-peers file"
        );
    }
}
