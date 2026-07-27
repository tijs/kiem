//! The sync half of the [`KiemStore`] facade: joining the iroh mesh, pairing
//! devices, and unpairing them.
//!
//! A second `#[uniffi::export] impl` block, not a second type — Swift still
//! sees one `KiemStore`. The split is for readers: the note/store surface next
//! door and this one share only the handle, and mixing 40 methods of both in
//! one file made neither easy to scan.
//!
//! Everything here goes through `kiem-sync`; nothing in this file touches a
//! note. The one piece of shared state is `KiemStore::sync`, the running
//! mesh's handle and the Tokio runtime that owns it — futures from that mesh
//! must be driven on that runtime, which is why several methods clone its
//! handle rather than build a new one.

use std::sync::Arc;
use std::time::Duration;

use crate::{sync_err, EventsAdapter, KiemError, KiemStore, PeerEvents, SyncHandle};

#[uniffi::export]
impl KiemStore {
    /// This device's stable identity (its iroh `EndpointId`, hex) — the id
    /// peers see on the mesh, and the value to pass as `author_did` when
    /// creating notes. Created on first use, persisted in the data dir.
    pub fn device_did(&self) -> Result<String, KiemError> {
        Ok(kiem_sync::device_id(&self.data_dir)
            .map_err(sync_err)?
            .to_string())
    }

    /// Binds this device's identity, accepts incoming connections, and dials
    /// every known peer. No-op if already running.
    pub fn start_sync(
        &self,
        interval_ms: u64,
        events: Arc<dyn PeerEvents>,
    ) -> Result<(), KiemError> {
        let mut sync = self.sync.lock().expect("sync lock poisoned");
        if sync.is_some() {
            return Ok(());
        }
        let runtime = tokio::runtime::Runtime::new().map_err(sync_err)?;
        let mesh = runtime
            .block_on(kiem_sync::Mesh::start(
                self.data_dir.clone(),
                self.state.clone(),
                Duration::from_millis(interval_ms.max(100)),
                Arc::new(EventsAdapter(events)),
            ))
            .map_err(sync_err)?;
        *sync = Some(SyncHandle { runtime, mesh });
        Ok(())
    }

    /// Stops the sync mesh. No-op if not running.
    pub fn stop_sync(&self) {
        if let Some(handle) = self.sync.lock().expect("sync lock poisoned").take() {
            handle.runtime.shutdown_background();
        }
    }

    /// This device's shareable pairing ticket, with a relay hint so the peer's
    /// first connect goes through the relay instead of paying cold discovery.
    /// When sync is running it's the live mesh endpoint's ticket; otherwise a
    /// standalone one. Both wait (bounded) for relay registration, so call this
    /// off the main thread.
    pub fn pair_ticket(&self) -> Result<String, KiemError> {
        // A running mesh's futures must run on the runtime that owns it.
        // Clone its handle so the relay wait doesn't hold the sync lock.
        let runtime = self
            .sync
            .lock()
            .expect("sync lock poisoned")
            .as_ref()
            .map(|handle| (handle.runtime.handle().clone(), handle.mesh.clone()));
        match runtime {
            Some((runtime, mesh)) => Ok(runtime.block_on(mesh.ticket_online())),
            None => tokio::runtime::Runtime::new()
                .map_err(sync_err)?
                .block_on(kiem_sync::pair_ticket(&self.data_dir))
                .map_err(sync_err),
        }
    }

    /// Opens the single-use pairing window for `window_secs`, during which one
    /// unknown peer may connect and (after approval) be trusted. No-op if sync
    /// isn't running.
    pub fn arm_pairing(&self, window_secs: u64) {
        if let Some(handle) = self.sync.lock().expect("sync lock poisoned").as_ref() {
            handle
                .mesh
                .arm_pairing(std::time::Duration::from_secs(window_secs));
        }
    }

    /// Whole seconds left on the open pairing window (rounded up), or `None`
    /// when closed — drives the app's countdown.
    pub fn pairing_window_remaining(&self) -> Option<u64> {
        let handle = self.sync.lock().expect("sync lock poisoned");
        handle
            .as_ref()?
            .mesh
            .pairing_window_remaining()
            .map(|d| d.as_secs() + u64::from(d.subsec_nanos() > 0))
    }

    /// Trusts the device behind a pasted/scanned ticket. If sync is running,
    /// forces an immediate pairing dial (bypassing the smaller-id-dials guard so
    /// it connects regardless of id ordering) and also starts the steady-state
    /// dial loop for ongoing reconnection.
    pub fn add_known_peer(&self, ticket: String) -> Result<String, KiemError> {
        let addr = kiem_sync::pair_add(&self.data_dir, &ticket).map_err(sync_err)?;
        let id = addr.id;
        if let Some(handle) = self.sync.lock().expect("sync lock poisoned").as_ref() {
            let _runtime = handle.runtime.enter();
            handle.mesh.pair_dial(addr.clone());
            handle.mesh.dial(addr);
        }
        Ok(id.to_string())
    }

    /// Unpairs a device — for a machine you no longer have. Drops it from the
    /// trust list (so it can neither dial in nor be dialed), forgets its name
    /// and its sync state, and closes any live session with it. Returns
    /// whether it was a known peer.
    ///
    /// Re-pairing later is a normal fresh pairing: nothing about the old link
    /// is kept, so the first sync after it re-handshakes from scratch.
    pub fn forget_known_peer(&self, peer_id: String) -> Result<bool, KiemError> {
        let id = peer_id.parse().map_err(|_| KiemError::Sync {
            message: format!("not a valid peer id: {peer_id}"),
        })?;
        match self.sync.lock().expect("sync lock poisoned").as_ref() {
            Some(handle) => handle.mesh.forget_peer(&id).map_err(sync_err),
            None => kiem_sync::forget(&self.data_dir, &self.state, &id).map_err(sync_err),
        }
    }

    /// Ids of every paired device (the known-peers file), whether or not it
    /// is currently reachable — the denominator for the sync-status UI.
    pub fn known_peers(&self) -> Result<Vec<String>, KiemError> {
        let peers = kiem_sync::KnownPeers::load(&self.data_dir.join(kiem_sync::PEERS_FILE))
            .map_err(sync_err)?;
        Ok(peers.ids().into_iter().map(|id| id.to_string()).collect())
    }

    /// Currently-connected peer ids, or empty if sync isn't running.
    pub fn connected_peers(&self) -> Vec<String> {
        match self.sync.lock().expect("sync lock poisoned").as_ref() {
            Some(handle) => handle
                .mesh
                .connected_ids()
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
            None => Vec::new(),
        }
    }

    /// Human-readable name for this device (defaults to the system host name).
    pub fn device_name(&self) -> String {
        kiem_sync::device_name(&self.data_dir)
    }

    /// Set the human-readable name for this device.
    pub fn set_device_name(&self, name: String) -> Result<(), KiemError> {
        kiem_sync::set_device_name(&self.data_dir, &name).map_err(sync_err)
    }

    /// Best-known human-readable name for a paired peer, or the peer id string
    /// if none has been recorded.
    pub fn peer_name(&self, peer_id: String) -> String {
        use std::str::FromStr;
        match kiem_sync::EndpointId::from_str(&peer_id) {
            Ok(id) => kiem_sync::peer_name(&self.data_dir, &id).unwrap_or_else(|| peer_id.clone()),
            Err(_) => peer_id,
        }
    }
}
