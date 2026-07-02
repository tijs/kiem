//! The peer mesh: bind an identity, accept incoming connections, keep dialing
//! known peers until they answer. Shared by every surface that wants "just
//! keep me synced with my paired devices" — the CLI daemon and the Swift
//! app's FFI bridge both drive one of these instead of each hand-rolling
//! their own accept/dial loop around [`session::run`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::Connection;

use crate::peers::{self, KnownPeers, PeersError};
use crate::session::{self, SessionError, SharedState};
use crate::{endpoint, identity, Endpoint, EndpointAddr, EndpointId, IdentityError};

pub const IDENTITY_FILE: &str = "identity.key";
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
}

/// Notified as peers connect/disconnect. Default no-ops so a caller that only
/// cares about one side (e.g. logging) doesn't have to implement both.
pub trait MeshEvents: Send + Sync + 'static {
    fn on_connected(&self, _peer: EndpointId) {}
    fn on_disconnected(&self, _peer: EndpointId) {}
}

/// A `MeshEvents` that does nothing, for callers that don't need events (the
/// CLI daemon logs from its own accept/dial call sites instead).
pub struct NoEvents;
impl MeshEvents for NoEvents {}

pub struct Mesh {
    endpoint: Endpoint,
    data_dir: PathBuf,
    state: SharedState,
    connected: Mutex<HashSet<EndpointId>>,
    interval: Duration,
    events: Arc<dyn MeshEvents>,
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
        let secret_key = identity::load_or_create(&data_dir.join(IDENTITY_FILE))?;
        let endpoint = endpoint::bind(secret_key).await?;

        let mesh = Arc::new(Mesh {
            endpoint,
            data_dir,
            state,
            connected: Mutex::new(HashSet::new()),
            interval,
            events,
        });

        tokio::spawn(accept_loop(mesh.clone()));
        let known = KnownPeers::load(&mesh.data_dir.join(PEERS_FILE))?;
        for id in known.ids().to_vec() {
            tokio::spawn(dial_loop(mesh.clone(), id));
        }
        Ok(mesh)
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.id()
    }

    pub fn connected_ids(&self) -> Vec<EndpointId> {
        self.connected.lock().unwrap().iter().copied().collect()
    }

    /// Starts dialing a newly-paired peer immediately, without waiting for
    /// the next process restart to pick it up from the known-peers file.
    pub fn dial(self: &Arc<Self>, id: EndpointId) {
        tokio::spawn(dial_loop(self.clone(), id));
    }
}

async fn accept_loop(mesh: Arc<Mesh>) {
    loop {
        match endpoint::accept(&mesh.endpoint).await {
            Ok(Some(connection)) => {
                let mesh = mesh.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(mesh, connection, false).await;
                });
            }
            Ok(None) => return, // endpoint closed
            Err(_) => continue,
        }
    }
}

/// Endless dial for a known peer — covers reconnection after a restart or a
/// network change, which is the entire point of moving off LAN-only mDNS.
async fn dial_loop(mesh: Arc<Mesh>, id: EndpointId) {
    loop {
        if !mesh.connected.lock().unwrap().contains(&id) {
            if let Ok(connection) = endpoint::connect(&mesh.endpoint, EndpointAddr::from(id)).await {
                let _ = handle_connection(mesh.clone(), connection, true).await;
            }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn handle_connection(
    mesh: Arc<Mesh>,
    connection: Connection,
    dialed: bool,
) -> Result<(), SessionError> {
    let peer = connection.remote_id();
    if !mesh.connected.lock().unwrap().insert(peer) {
        return Ok(()); // already linked to this peer (a dial/accept race)
    }
    mesh.events.on_connected(peer);
    let result = session::run(connection, dialed, mesh.state.clone(), mesh.interval).await;
    mesh.connected.lock().unwrap().remove(&peer);
    mesh.events.on_disconnected(peer);
    result
}

/// This device's shareable ticket, without needing a `Mesh` already running
/// (a fresh device pairs before it has ever synced). Binds a short-lived
/// endpoint just long enough to read its address.
pub async fn pair_ticket(data_dir: &Path) -> Result<String, MeshError> {
    let secret_key = identity::load_or_create(&data_dir.join(IDENTITY_FILE))?;
    let endpoint = endpoint::bind(secret_key).await?;
    let ticket = peers::my_ticket(&endpoint).to_string();
    endpoint.close().await;
    Ok(ticket)
}

/// Trusts the device behind a pasted/scanned ticket, persisting it to the
/// known-peers file. Does not require a `Mesh` to be running.
pub fn pair_add(data_dir: &Path, ticket: &str) -> Result<EndpointId, MeshError> {
    let addr = peers::parse_ticket(ticket)?;
    let peers_path = data_dir.join(PEERS_FILE);
    let mut known = KnownPeers::load(&peers_path)?;
    known.add(&peers_path, addr.id)?;
    Ok(addr.id)
}
