//! Human-readable device names for the sync mesh. Each device stores its own
//! name (defaulting to the system host name) and remembers the names it learns
//! from peers during pairing.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use iroh::EndpointId;

pub(crate) const DEVICE_NAME_FILE: &str = "device-name";
pub(crate) const PEER_NAMES_FILE: &str = "peer-names";

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
    write_private_file(&path, trimmed.as_bytes())
}

/// The remembered display name for a peer, if any.
pub fn peer_name(data_dir: &Path, peer_id: &EndpointId) -> Option<String> {
    peer_names(data_dir).get(&peer_id.to_string()).cloned()
}

/// Remember a peer's display name. Empty names are ignored.
pub(crate) fn set_peer_name(
    data_dir: &Path,
    peer_id: &EndpointId,
    name: &str,
) -> std::io::Result<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let mut map = peer_names(data_dir);
    map.insert(peer_id.to_string(), trimmed.to_owned());
    save_peer_names(data_dir, &map)
}

/// Forget a peer's remembered name (part of unpairing). No-op if unknown.
pub(crate) fn forget_peer_name(data_dir: &Path, peer_id: &EndpointId) -> std::io::Result<()> {
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
    let contents =
        serde_json::to_string_pretty(names).expect("a map of strings always serializes to JSON");
    write_private_file(&path, contents.as_bytes())
}

/// Writes metadata that may reveal paired-device information. On Unix, both
/// fresh and pre-existing files are forced to `0600`; non-Unix uses native ACLs.
fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        crate::storage::ensure_private_data_dir(parent)?;
    }
    let mut options = File::options();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(contents)?;
    file.sync_all()
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

    #[cfg(unix)]
    #[test]
    fn device_and_peer_name_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let peer = SecretKey::generate().public();
        set_device_name(dir.path(), "My Mac").unwrap();
        set_peer_name(dir.path(), &peer, "Other Mac").unwrap();

        for path in [
            dir.path().join(DEVICE_NAME_FILE),
            dir.path().join(PEER_NAMES_FILE),
        ] {
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600,
                "{} must not be group/other-readable",
                path.display()
            );
        }
    }

    #[test]
    fn forgetting_a_peer_name_removes_only_that_peer() {
        let dir = tempfile::tempdir().unwrap();
        let (gone, kept) = (
            SecretKey::generate().public(),
            SecretKey::generate().public(),
        );
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
