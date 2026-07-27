//! Shared setup for the integration tests that stand up real iroh endpoints.

#![allow(dead_code)] // each test binary uses a different subset

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use iroh::{endpoint::presets, Endpoint, SecretKey};
use kiem_core::store::NoteStore;
use kiem_sync::{PeerHandshake, SharedState};

/// A genuinely loopback endpoint speaking kiem's ALPN: `127.0.0.1` only, no
/// relay, no n0 DNS publishing.
///
/// Deliberately *not* `kiem_sync::bind`, which applies the `N0` preset the
/// product needs. Under that preset a test dials over whatever real interfaces
/// the machine happens to have, and `connect` spent **~9s** timing out dead
/// paths before falling back to a working address — `Endpoint::addr()`
/// advertises every interface, and here a Tailscale `100.x` address sorted
/// ahead of the LAN one. Pinned to `127.0.0.1` the same connect takes **~8ms**.
/// Dropping n0 discovery also drops an external dependency the sync protocol
/// never needed. Nothing in `session::run` cares how the peers found each
/// other, so this gives up no coverage.
pub async fn bind_loopback() -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::generate())
        .alpns(vec![kiem_sync::ALPN.to_vec()])
        .clear_ip_transports()
        .bind_addr("127.0.0.1:0")
        .expect("127.0.0.1:0 is a valid bind address")
        .bind()
        .await
        .expect("binding a loopback iroh endpoint")
}

/// Pay iroh's one-time, process-global network-stack initialisation up front.
///
/// The first `Endpoint::bind` in a process initialises shared state (the
/// interface monitor); every bind after it is ~20ms. Measured cost of that
/// first one: **~5s** on an idle machine and **44.6s** while the rest of the
/// workspace suite competes for CPU — and concurrent first binds all return at
/// the same instant, so it is one shared init rather than per-endpoint work.
///
/// It belongs to no test, so tests pay it *before* starting their own clock.
/// Left inside the budget it made a plain `cargo test` report "no local
/// networking" purely because another crate's tests were busy — which is how
/// this suite came to look permanently broken on a healthy machine. A machine
/// that genuinely cannot bind still fails here, with a `BindError` that says
/// so instead of an unexplained timeout.
pub async fn warm_network_stack() {
    drop(bind_loopback().await);
}

/// A session's shared store + engine, backed by an in-memory store.
pub fn empty_state() -> SharedState {
    kiem_sync::shared_state(NoteStore::open_in_memory_with_search().unwrap())
}

/// A handshake that only counts sync activity — the callbacks most tests
/// don't assert on.
pub fn handshake(ticket: String, name: &str, activity: Arc<AtomicUsize>) -> PeerHandshake {
    PeerHandshake {
        local_ticket: ticket,
        local_name: name.to_owned(),
        on_peer: Arc::new(|_addr| {}),
        on_name: Arc::new(|_peer, _name| {}),
        on_sync_activity: Arc::new(move |_peer| {
            activity.fetch_add(1, Ordering::Relaxed);
        }),
    }
}

/// Aborts a spawned task when dropped, so a live iroh session (and its UDP
/// socket) doesn't leak if a later assertion panics before the test's own
/// explicit cleanup would otherwise run.
pub struct AbortOnDrop<T>(pub tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
