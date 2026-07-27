//! Human-readable device names for the sync mesh. Each device stores its own
//! name (defaulting to the system host name) and remembers the names it learns
//! from peers during pairing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use iroh::EndpointId;

pub const DEVICE_NAME_FILE: &str = "device-name";
pub const PEER_NAMES_FILE: &str = "peer-names";

/// The local device's display name, read from `data_dir/device-name`. Falls back
/// to the OS host name; if even that fails, returns the bare peer id prefix.
/// ponytail: no config UI yet, so a host-name default is good enough.
pub fn device_name(data_dir: &Path) -> String {
    let path = data_dir.join(DEVICE_NAME_FILE);
    if let Ok(contents) = std::fs::read_to_string(&path) {
        let trimmed = contents.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "Kiem Device".to_owned())
}

/// Persist the local device name. Empty names are ignored.
pub fn set_device_name(data_dir: &Path, name: &str) -> std::io::Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let path = data_dir.join(DEVICE_NAME_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, trimmed.as_bytes())
}

/// The remembered display name for a peer, if any.
pub fn peer_name(data_dir: &Path, peer_id: &EndpointId) -> Option<String> {
    peer_names(data_dir).get(&peer_id.to_string()).cloned()
}

/// Remember a peer's display name. Empty names are ignored.
pub fn set_peer_name(data_dir: &Path, peer_id: &EndpointId, name: &str) -> std::io::Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let mut map = peer_names(data_dir);
    map.insert(peer_id.to_string(), trimmed.to_owned());
    save_peer_names(data_dir, &map)
}

/// Forget a peer's remembered name (part of unpairing). No-op if unknown.
pub fn forget_peer_name(data_dir: &Path, peer_id: &EndpointId) -> std::io::Result<()> {
    let mut map = peer_names(data_dir);
    if map.remove(&peer_id.to_string()).is_none() {
        return Ok(());
    }
    save_peer_names(data_dir, &map)
}

fn peer_names_path(data_dir: &Path) -> PathBuf {
    data_dir.join(PEER_NAMES_FILE)
}

fn peer_names(data_dir: &Path) -> HashMap<String, String> {
    let path = peer_names_path(data_dir);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_peer_names(data_dir: &Path, names: &HashMap<String, String>) -> std::io::Result<()> {
    let path = peer_names_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(names)
            .unwrap_or_default()
            .as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    #[test]
    fn round_trip_local_and_peer_names() {
        let dir = tempfile::tempdir().unwrap();
        let peer_id = SecretKey::generate().public();

        set_device_name(dir.path(), "My Mac").unwrap();
        assert_eq!(device_name(dir.path()), "My Mac");

        set_peer_name(dir.path(), &peer_id, "Other Mac").unwrap();
        assert_eq!(
            peer_name(dir.path(), &peer_id),
            Some("Other Mac".to_owned())
        );
    }

    #[test]
    fn forgetting_a_peer_name_removes_only_that_peer() {
        let dir = tempfile::tempdir().unwrap();
        let (gone, kept) = (SecretKey::generate().public(), SecretKey::generate().public());
        set_peer_name(dir.path(), &gone, "Old Mac").unwrap();
        set_peer_name(dir.path(), &kept, "Other Mac").unwrap();

        forget_peer_name(dir.path(), &gone).unwrap();

        assert_eq!(peer_name(dir.path(), &gone), None);
        assert_eq!(peer_name(dir.path(), &kept), Some("Other Mac".to_owned()));
        // Unknown peer: no-op, not an error.
        forget_peer_name(dir.path(), &gone).unwrap();
    }

    #[test]
    fn empty_names_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let peer_id = SecretKey::generate().public();

        set_device_name(dir.path(), "   ").unwrap();
        // Falls back to host name, which is non-empty.
        assert!(!device_name(dir.path()).trim().is_empty());

        set_peer_name(dir.path(), &peer_id, "   ").unwrap();
        assert_eq!(peer_name(dir.path(), &peer_id), None);
    }
}
