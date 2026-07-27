//! The peer mesh: bind an identity, accept incoming connections, keep dialing
//! known peers until they answer. Shared by every surface that wants "just
//! keep me synced with my paired devices" — the CLI daemon and the Swift
//! app's FFI bridge both drive one of these instead of each hand-rolling
//! their own accept/dial loop around [`session::run`].
//!
//! This file owns the connection lifecycle. The three concerns around it live
//! next door: [`gate`] decides who may connect (the trust boundary),
//! [`pairing`] adds and removes peers without needing a mesh at all, and
//! [`lock`] keeps two meshes off one data dir.

mod gate;
mod lock;
mod pairing;

pub use pairing::{forget, pair_add, pair_ticket};

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::Connection;

use crate::names::{device_name, set_peer_name};
use crate::peers::{self, KnownPeers, PeersError};
use crate::session::{self, PeerHandshake, SessionError, SharedState};
use crate::{endpoint, identity, Endpoint, EndpointAddr, EndpointId, IdentityError};

pub const PEERS_FILE: &str = "known-peers";
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

#[derive(thiserror::Error, Debug)]
pub enum MeshError {
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error(transparent)]
    Endpoint(#[from] crate::EndpointError),
    #[error(transparent)]
    Peers(#[from] PeersError),
    /// Another process (CLI daemon or app) already holds the mesh lock for
    /// this data dir — two accept/dial loops on one identity corrupt
    /// discovery, so only one may run at a time.
    #[error("another kiem process is already syncing {data_dir}")]
    AlreadyRunning { data_dir: String },
    #[error("locking {path}: {source}")]
    Lock { path: String, source: std::io::Error },
}

/// Notified as peers connect/disconnect. Default no-ops so a caller that only
/// cares about one side (e.g. logging) doesn't have to implement both.
pub trait MeshEvents: Send + Sync + 'static {
    fn on_connected(&self, _peer: EndpointId) {}
    fn on_disconnected(&self, _peer: EndpointId) {}
    /// A sync message was just sent to or received from the peer. Use it to
    /// drive a transient "syncing" indicator in the UI.
    fn on_sync_activity(&self, _peer: EndpointId) {}
    /// A connect/accept attempt failed. `context` is e.g. "accept" or a
    /// dialed peer id; non-fatal — the mesh keeps retrying.
    fn on_error(&self, _context: &str, _error: &str) {}
    /// An unknown peer dialed in during an open pairing window — approve
    /// trusting it? Called on a blocking thread, so an implementation may wait
    /// on a user prompt. Default-deny: a caller that doesn't override this never
    /// pairs a stranger, only known peers.
    fn approve_pairing(&self, _peer: EndpointId) -> bool {
        false
    }
}

/// A `MeshEvents` that does nothing, for callers that don't need events (the
/// CLI daemon logs from its own accept/dial call sites instead).
pub struct NoEvents;
impl MeshEvents for NoEvents {}

pub struct Mesh {
    /// Held for the life of the mesh; never read, only kept alive so its
    /// `Drop` releases the lock (see [`lock::acquire`]).
    _lock: File,
    endpoint: Endpoint,
    data_dir: PathBuf,
    state: SharedState,
    /// Live sessions, keyed by peer. The `Connection` is kept (not just the
    /// id) so unpairing can close the link immediately instead of leaving a
    /// discarded device syncing until it happens to drop.
    connected: Mutex<HashMap<EndpointId, Connection>>,
    interval: Duration,
    events: Arc<dyn MeshEvents>,
    /// Peers with a live guarded dial loop, so we start exactly one per peer
    /// (a fresh pairing would otherwise spawn a duplicate every reconnect).
    dialing: Mutex<HashSet<EndpointId>>,
    /// Deadline of the open pairing window, or `None` when closed. An unknown
    /// peer is only admitted while this is `Some` and unexpired, and admitting
    /// one closes it (single-use).
    pairing_until: Mutex<Option<Instant>>,
}

impl Mesh {
    /// Binds this device's identity, starts accepting connections, and dials
    /// every currently-known peer. Returns once listening — dialing and
    /// accepting continue on spawned tasks for the life of the returned
    /// `Arc`.
    pub async fn start(
        data_dir: PathBuf,
        state: SharedState,
        interval: Duration,
        events: Arc<dyn MeshEvents>,
    ) -> Result<Arc<Mesh>, MeshError> {
        let lock = lock::acquire(&data_dir)?;
        let secret_key = identity::load_or_create(&data_dir.join(identity::IDENTITY_FILE))?;
        let endpoint = endpoint::bind(secret_key).await?;
        Self::assemble(lock, endpoint, data_dir, state, interval, events)
    }

    /// [`start`](Self::start) on an endpoint the caller bound itself — its
    /// secret key stands in for the on-disk identity, so nothing is read from
    /// or written to `data_dir` except the lock and the known-peers file.
    ///
    /// Exists so a caller can choose the endpoint's transport configuration.
    /// The integration tests bind loopback-only: on a dev machine the addresses
    /// in a freshly-read ticket are the LAN and Tailscale ones, and dialing
    /// those hairpins back to the same host, which simply times out — pairing
    /// tests then "passed" without a connection ever forming.
    pub async fn start_with_endpoint(
        endpoint: Endpoint,
        data_dir: PathBuf,
        state: SharedState,
        interval: Duration,
        events: Arc<dyn MeshEvents>,
    ) -> Result<Arc<Mesh>, MeshError> {
        let lock = lock::acquire(&data_dir)?;
        Self::assemble(lock, endpoint, data_dir, state, interval, events)
    }

    /// Wires up a `Mesh` on an already-bound endpoint and held lock, and starts
    /// its accept loop and one dial loop per already-known peer.
    fn assemble(
        lock: File,
        endpoint: Endpoint,
        data_dir: PathBuf,
        state: SharedState,
        interval: Duration,
        events: Arc<dyn MeshEvents>,
    ) -> Result<Arc<Mesh>, MeshError> {
        let mesh = Arc::new(Mesh {
            _lock: lock,
            endpoint,
            data_dir,
            state,
            connected: Mutex::new(HashMap::new()),
            interval,
            events,
            dialing: Mutex::new(HashSet::new()),
            pairing_until: Mutex::new(None),
        });

        tokio::spawn(accept_loop(mesh.clone()));
        let known = KnownPeers::load(&mesh.data_dir.join(PEERS_FILE))?;
        for addr in known.addrs().to_vec() {
            mesh.dial(addr);
        }
        Ok(mesh)
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// This running mesh's shareable pairing ticket (its address as a ticket
    /// string). Unlike the standalone [`pair_ticket`], it reflects the live
    /// endpoint that's actually accepting connections. May carry no relay hint
    /// if read before relay registration — prefer [`ticket_online`] for a code
    /// shown to a user.
    pub fn ticket(&self) -> String {
        peers::my_ticket(&self.endpoint).to_string()
    }

    /// Like [`ticket`] but first waits (bounded) for relay registration, so the
    /// ticket carries a relay hint and the peer's first connect goes through the
    /// relay immediately instead of paying 20-35s of cold discovery (the
    /// df5ddfeb finding). Use this for a ticket shown to a user to pair with.
    pub async fn ticket_online(&self) -> String {
        let _ = tokio::time::timeout(pairing::TICKET_RELAY_WAIT, self.endpoint.online()).await;
        self.ticket()
    }

    pub fn connected_ids(&self) -> Vec<EndpointId> {
        self.connected.lock().unwrap().keys().copied().collect()
    }

    /// Unpairs a device: drops it from the trust list and from this mesh's
    /// live state, and closes any session with it right now. Returns whether
    /// it was a known peer.
    ///
    /// Order matters. The trust list is written *first*, so by the time the
    /// connection closes the peer is already untrusted: its dial loop stops
    /// (see [`dial_loop`]) instead of reconnecting two seconds later, and an
    /// incoming dial from it is refused by the gate in [`admit_incoming`].
    pub fn forget_peer(&self, peer: &EndpointId) -> Result<bool, MeshError> {
        let known = forget(&self.data_dir, &self.state, peer)?;
        if let Some(connection) = self.connected.lock().unwrap().remove(peer) {
            connection.close(0u32.into(), b"unpaired");
        }
        Ok(known)
    }

    /// Starts (at most one) guarded dial loop for a peer — reconnects it for the
    /// life of the mesh. Idempotent: calling it again for a peer that already
    /// has a loop is a no-op, so it's safe to call on every pairing.
    pub fn dial(self: &Arc<Self>, addr: EndpointAddr) {
        if !self.dialing.lock().unwrap().insert(addr.id) {
            return;
        }
        tokio::spawn(dial_loop(self.clone(), addr));
    }

    /// Dials a just-added peer once, right now, *bypassing* the steady-state
    /// "only the smaller EndpointId dials" guard. Pairing needs this: the show
    /// side doesn't know us yet, so it can't dial, and if we're the larger id
    /// the guarded [`dial`] loop would never dial either — the pairing
    /// connection would simply never form. Ongoing reconnection stays with the
    /// guarded loop / next startup; this is only the first-contact nudge.
    pub fn pair_dial(self: &Arc<Self>, addr: EndpointAddr) {
        tokio::spawn(pair_dial_once(self.clone(), addr));
    }

    /// The pairing handshake for one connection: our own ticket to send, and a
    /// recorder that adds the peer's (id-checked) address to the known-peers
    /// file. Adding is a no-op for an already-trusted peer.
    ///
    /// ponytail: the known-peers file has no cross-connection lock, so two
    /// simultaneous first-contacts could race on the write — harmless for a
    /// handful of personal devices; add a file lock if that ever changes.
    fn peer_handshake(self: &Arc<Self>) -> PeerHandshake {
        let peers_path = self.data_dir.join(PEERS_FILE);
        let data_dir = self.data_dir.clone();
        let local_name = device_name(&data_dir);
        PeerHandshake {
            local_ticket: peers::my_ticket(&self.endpoint).to_string(),
            local_name,
            on_peer: {
                let weak = Arc::downgrade(self);
                Arc::new(move |addr: EndpointAddr| {
                    // Record the peer so we can reach it again. A write failure here
                    // means a just-paired device silently wouldn't be remembered —
                    // surface it rather than swallow it.
                    match KnownPeers::load(&peers_path)
                        .and_then(|mut known| known.add(&peers_path, addr.clone()))
                    {
                        Ok(()) => {}
                        Err(err) => {
                            if let Some(mesh) = weak.upgrade() {
                                mesh.events.on_error("pair-record", &err.to_string());
                            }
                        }
                    }
                    // Keep the link alive after pairing: start a guarded dial loop
                    // for the peer (it dials or no-ops by id ordering, deduped by
                    // `dialing`), so a dropped connection reconnects without waiting
                    // for a restart — including when this (show) side is the smaller
                    // id and would otherwise never dial.
                    if let Some(mesh) = weak.upgrade() {
                        mesh.dial(addr);
                    }
                })
            },
            on_name: {
                let weak = Arc::downgrade(self);
                Arc::new(move |peer: EndpointId, name: String| {
                    if let Some(mesh) = weak.upgrade() {
                        if let Err(err) = set_peer_name(&mesh.data_dir, &peer, &name) {
                            mesh.events.on_error("peer-name", &err.to_string());
                        }
                    }
                })
            },
            on_sync_activity: {
                let weak = Arc::downgrade(self);
                Arc::new(move |peer: EndpointId| {
                    if let Some(mesh) = weak.upgrade() {
                        mesh.events.on_sync_activity(peer);
                    }
                })
            },
        }
    }
}

async fn accept_loop(mesh: Arc<Mesh>) {
    loop {
        match endpoint::accept(&mesh.endpoint).await {
            Ok(Some(connection)) => {
                let mesh = mesh.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_connection(mesh.clone(), connection, false).await {
                        mesh.events.on_error("accept", &err.to_string());
                    }
                });
            }
            Ok(None) => return, // endpoint closed
            Err(err) => mesh.events.on_error("accept", &err.to_string()),
        }
    }
}

/// Endless dial for a known peer — covers reconnection after a restart or a
/// network change, which is the entire point of moving off LAN-only mDNS.
///
/// Only the lexicographically smaller `EndpointId` dials; the other side just
/// accepts. Without this, both known-peers would dial each other
/// simultaneously, establishing two independent connections where each side
/// commits to a different one of the two — so `open_bi`/`accept_bi` never
/// pair up on either connection and nothing ever syncs, silently.
async fn dial_loop(mesh: Arc<Mesh>, addr: EndpointAddr) {
    let id = addr.id;
    if mesh.endpoint.id() >= id {
        // We're the accepting side for this peer — release the dial slot so a
        // later `dial` for it isn't wrongly suppressed as a duplicate.
        mesh.dialing.lock().unwrap().remove(&id);
        return;
    }
    loop {
        // Unpairing is the loop's only exit: without this it would redial a
        // forgotten device every two seconds forever. Reading the trust list
        // (rather than a flag) also catches an unpair done by another process
        // — `kiem pair forget` while the app holds the mesh.
        if !mesh.is_known(&id) {
            mesh.dialing.lock().unwrap().remove(&id);
            return;
        }
        if !mesh.connected.lock().unwrap().contains_key(&id) {
            match endpoint::connect(&mesh.endpoint, addr.clone()).await {
                Ok(connection) => {
                    if let Err(err) = handle_connection(mesh.clone(), connection, true).await {
                        mesh.events.on_error(&id.to_string(), &err.to_string());
                    }
                }
                Err(err) => mesh.events.on_error(&id.to_string(), &err.to_string()),
            }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// One forced pairing dial (no ordering guard, no reconnect loop). See
/// [`Mesh::pair_dial`].
async fn pair_dial_once(mesh: Arc<Mesh>, addr: EndpointAddr) {
    let id = addr.id;
    if mesh.connected.lock().unwrap().contains_key(&id) {
        return; // the guarded loop already linked this peer
    }
    match endpoint::connect(&mesh.endpoint, addr).await {
        Ok(connection) => {
            if let Err(err) = handle_connection(mesh.clone(), connection, true).await {
                mesh.events.on_error(&id.to_string(), &err.to_string());
            }
        }
        Err(err) => mesh.events.on_error(&id.to_string(), &err.to_string()),
    }
}

async fn handle_connection(
    mesh: Arc<Mesh>,
    connection: Connection,
    dialed: bool,
) -> Result<(), SessionError> {
    let peer = connection.remote_id();
    // The trust gate. Runs on a blocking thread because approving an unknown
    // peer can wait on a user prompt; dropping the connection on refusal closes
    // it before any sync happens.
    let admit = {
        let mesh = mesh.clone();
        tokio::task::spawn_blocking(move || mesh.admit_incoming(peer, dialed))
            .await
            .unwrap_or(false)
    };
    if !admit {
        return Ok(());
    }
    {
        let mut connected = mesh.connected.lock().unwrap();
        if connected.contains_key(&peer) {
            return Ok(()); // already linked to this peer (a dial/accept race)
        }
        connected.insert(peer, connection.clone());
    }
    mesh.events.on_connected(peer);
    let result = session::run(
        connection,
        dialed,
        mesh.state.clone(),
        mesh.interval,
        mesh.peer_handshake(),
    )
    .await;
    mesh.connected.lock().unwrap().remove(&peer);
    mesh.events.on_disconnected(peer);
    result
}
