//! iroh-based P2P transport for Kiem. Owns device identity, discovery, relay
//! fallback, and the async runtime — kept out of `kiem-core` so that crate
//! stays a dependency-light, publishable library. Feeds connections to
//! `kiem-core`'s `SyncEngine`, which is unaware of iroh entirely.

mod endpoint;
mod identity;
mod mesh;
mod names;
mod peers;
mod session;

pub use endpoint::{accept, bind, connect, EndpointError, ALPN};
pub use identity::{device_id, load_or_create, IdentityError};
pub use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
pub use mesh::{forget, pair_add, pair_ticket, Mesh, MeshEvents, NoEvents, PEERS_FILE};
pub use names::{
    device_name, forget_peer_name, peer_name, set_device_name, set_peer_name, DEVICE_NAME_FILE,
    PEER_NAMES_FILE,
};
pub use peers::{my_ticket, parse_ticket, KnownPeers, PeersError};
pub use session::{run as run_session, shared_state, PeerHandshake, SessionError, SharedState};
