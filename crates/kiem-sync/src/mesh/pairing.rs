//! Trust-list operations that do **not** need a running mesh — pairing a
//! device and unpairing one, straight at the files in the data dir.
//!
//! They exist because both happen outside a sync session: a fresh device pairs
//! before it has ever synced, and `kiem pair forget` may run while no daemon
//! is up. When a mesh *is* running it has strictly more to do (dial the new
//! peer, close the connection to the old one), so it wraps these — see
//! [`Mesh::dial`] and [`Mesh::forget_peer`].

use std::path::Path;
use std::time::Duration;

use super::{MeshError, PEERS_FILE};
use crate::names::forget_peer_name;
use crate::peers::{self, KnownPeers};
use crate::session::SharedState;
use crate::{endpoint, identity, EndpointAddr, EndpointId};

/// How long ticket generation waits for relay registration before settling
/// for a relay-less ticket (an offline machine must still be able to pair).
pub(super) const TICKET_RELAY_WAIT: Duration = Duration::from_secs(10);

/// This device's shareable ticket, without needing a `Mesh` already running
/// (a fresh device pairs before it has ever synced). Binds a short-lived
/// endpoint and waits for relay registration first: an address read straight
/// after bind carries no relay URL, which forces the peer to dial by bare
/// EndpointId — 20–35s of cold discovery (the df5ddfeb finding). With the
/// relay hint in the ticket, the first connect goes through the relay
/// immediately and upgrades to direct.
pub async fn pair_ticket(data_dir: &Path) -> Result<String, MeshError> {
    let secret_key = identity::load_or_create(&data_dir.join(identity::IDENTITY_FILE))?;
    let endpoint = endpoint::bind(secret_key).await?;
    let _ = tokio::time::timeout(TICKET_RELAY_WAIT, endpoint.online()).await;
    let ticket = peers::my_ticket(&endpoint).to_string();
    endpoint.close().await;
    Ok(ticket)
}

/// Trusts the device behind a pasted/scanned ticket, persisting it to the
/// known-peers file. Returns the full address (not just the id) so a caller
/// can dial it immediately via [`Mesh::dial`].
///
/// [`Mesh::dial`]: super::Mesh::dial
pub fn pair_add(data_dir: &Path, ticket: &str) -> Result<EndpointAddr, MeshError> {
    let addr = peers::parse_ticket(ticket)?;
    let peers_path = data_dir.join(PEERS_FILE);
    let mut known = KnownPeers::load(&peers_path)?;
    known.add(&peers_path, addr.clone())?;
    Ok(addr)
}

/// Unpairs a device: drops it from the known-peers file, forgets its
/// remembered name, and drops its sync state — the last of which is what stops
/// a long-lived process holding a discarded device's per-document states (see
/// [`SyncEngine::forget_peer`]). Returns whether it was a known peer.
///
/// Callers that have a mesh should use [`Mesh::forget_peer`], which does this
/// *and* closes the live connection.
///
/// [`SyncEngine::forget_peer`]: kiem_core::sync::SyncEngine::forget_peer
/// [`Mesh::forget_peer`]: super::Mesh::forget_peer
pub fn forget(data_dir: &Path, state: &SharedState, peer: &EndpointId) -> Result<bool, MeshError> {
    let peers_path = data_dir.join(PEERS_FILE);
    let mut known = KnownPeers::load(&peers_path)?;
    let was_known = known.remove(&peers_path, peer)?;
    // Best-effort: a stale display name is cosmetic, and failing the unpair
    // over it would leave the device trusted, which is the part that matters.
    let _ = forget_peer_name(data_dir, peer);
    state.lock().1.forget_peer(&peer.to_string());
    Ok(was_known)
}
