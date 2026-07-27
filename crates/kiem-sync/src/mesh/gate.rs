//! The trust boundary: who is allowed to sync with this device.
//!
//! Two rules, and everything in this file exists to keep them together where
//! they can be read as a pair:
//!
//! - a peer in the known-peers file may always connect;
//! - anyone else may connect only during an open pairing window, once, and
//!   only if the caller's `approve_pairing` says yes.
//!
//! Kept out of `mesh.rs` deliberately — this is the security-relevant surface,
//! and it should not have to be reconstructed from among the dial loops.

use std::time::{Duration, Instant};

use super::{KnownPeers, Mesh, PEERS_FILE};
use crate::EndpointId;

impl Mesh {
    /// Opens the single-use pairing window for `window`, during which one
    /// unknown peer may connect and be trusted. Re-arming replaces any prior
    /// deadline.
    pub fn arm_pairing(&self, window: Duration) {
        *self.pairing_until.lock().unwrap() = Some(Instant::now() + window);
    }

    /// Time left on the open pairing window, or `None` when closed/expired
    /// (drives the app's countdown). Reading an expired window closes it.
    pub fn pairing_window_remaining(&self) -> Option<Duration> {
        let mut until = self.pairing_until.lock().unwrap();
        match *until {
            Some(deadline) => deadline.checked_duration_since(Instant::now()).or_else(|| {
                *until = None;
                None
            }),
            None => None,
        }
    }

    /// Whether an incoming (accepted) connection from `peer` may proceed. Known
    /// peers always do; an unknown peer is admitted only during an open pairing
    /// window *and* an approved prompt, which then consumes the window (a denied
    /// attempt leaves it open for the real device). `dialed` connections are
    /// ours (we only ever dial peers we already trust), so they skip the gate.
    ///
    /// The window lock is never held across `approve_pairing` — that call can
    /// block on a user prompt, and the countdown UI reads the window meanwhile.
    pub(super) fn admit_incoming(&self, peer: EndpointId, dialed: bool) -> bool {
        if dialed || self.is_known(&peer) {
            return true;
        }
        {
            let mut until = self.pairing_until.lock().unwrap();
            if !window_open(&mut until, Instant::now()) {
                return false;
            }
        }
        if !self.events.approve_pairing(peer) {
            return false; // denied — leave the window open for the real device
        }
        // Approved: consume the window (single-use), re-checking it didn't lapse
        // while the prompt was up.
        let mut until = self.pairing_until.lock().unwrap();
        let admit = window_open(&mut until, Instant::now());
        *until = None;
        admit
    }

    /// Is this peer in the trust list? Read from the file every time, not
    /// cached: it is also what makes an unpair performed by another process
    /// take effect here (see `Mesh::forget_peer` and the dial loop).
    pub(super) fn is_known(&self, peer: &EndpointId) -> bool {
        KnownPeers::load(&self.data_dir.join(PEERS_FILE))
            .map(|known| known.contains(peer))
            .unwrap_or(false)
    }
}

/// Is the pairing window open at `now`? Clears an expired deadline in place.
/// Pure — the gate's window check has to be right (it's the trust boundary), so
/// it's tested without needing a live endpoint. It does *not* consume an open
/// window; only an approved pairing does (see `admit_incoming`).
fn window_open(window: &mut Option<Instant>, now: Instant) -> bool {
    match *window {
        Some(deadline) if now < deadline => true,
        _ => {
            *window = None;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_open_is_true_only_within_the_deadline_and_clears_when_lapsed() {
        let now = Instant::now();

        // Open: true, and left open (checking doesn't consume it — a denied
        // pairing must leave the window for the real device).
        let mut open = Some(now + Duration::from_secs(60));
        assert!(window_open(&mut open, now));
        assert!(open.is_some(), "checking an open window must not close it");

        // Closed: false.
        let mut closed = None;
        assert!(!window_open(&mut closed, now));

        // Expired: false, and cleared in place.
        let mut expired = Some(now - Duration::from_secs(1));
        assert!(!window_open(&mut expired, now));
        assert!(expired.is_none(), "an expired window must be cleared");
    }
}
