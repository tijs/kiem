//! iroh-based P2P transport for Kiem. Owns device identity, discovery, relay
//! fallback, and the async runtime — kept out of `kiem-core` so that crate
//! stays a dependency-light, publishable library. Feeds connections to
//! `kiem-core`'s `SyncEngine`, which is unaware of iroh entirely.

mod endpoint;
mod identity;
mod peers;
mod session;

pub use endpoint::{accept, bind, connect, EndpointError, ALPN};
pub use identity::{load_or_create, IdentityError};
pub use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
pub use peers::{my_ticket, parse_ticket, KnownPeers, PeersError};
pub use session::{run as run_session, SessionError, SharedState};
