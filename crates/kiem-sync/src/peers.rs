use std::fs::{self, File, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_tickets::endpoint::EndpointTicket;

#[derive(thiserror::Error, Debug)]
pub enum PeersError {
    #[error("reading known-peers file at {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("writing known-peers file at {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[error("locking known-peers updates at {path}: {source}")]
    Lock {
        path: String,
        source: std::io::Error,
    },
    #[error("known-peers file at {path} has an invalid ticket on line {line}")]
    Corrupt { path: String, line: usize },
    #[error("invalid ticket: {0}")]
    InvalidTicket(#[from] iroh_tickets::ParseError),
}

/// A device's trust list, replacing mDNS's "any peer on the LAN" auto-discovery
/// with an explicit, one-time pairing step. Persisted as one ticket per line
/// (not just the bare id) — a ticket carries relay/direct-address hints that
/// let `connect` skip a cold discovery lookup, which otherwise adds several
/// seconds (or fails outright while a peer's Pkarr/DNS record is still
/// propagating) — see the mesh-flakiness fix this store shipped alongside.
pub struct KnownPeers {
    addrs: Vec<EndpointAddr>,
}

impl KnownPeers {
    pub fn load(path: &Path) -> Result<Self, PeersError> {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let mut addrs = Vec::new();
                for (line_no, line) in contents.lines().enumerate() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let ticket: EndpointTicket = line.parse().map_err(|_| PeersError::Corrupt {
                        path: path.display().to_string(),
                        line: line_no + 1,
                    })?;
                    addrs.push(ticket.endpoint_addr().clone());
                }
                Ok(Self { addrs })
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self { addrs: Vec::new() })
            }
            Err(source) => Err(PeersError::Read {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    pub fn contains(&self, id: &EndpointId) -> bool {
        self.addrs.iter().any(|a| &a.id == id)
    }

    pub fn ids(&self) -> Vec<EndpointId> {
        self.addrs.iter().map(|a| a.id).collect()
    }

    pub fn addrs(&self) -> &[EndpointAddr] {
        &self.addrs
    }

    /// Adds a peer (no-op if already known) and persists the updated list.
    ///
    /// The file is reloaded while holding the narrowly-scoped peer-store lock,
    /// rather than trusting `self`: callers commonly loaded `self` before a
    /// separate CLI or first-contact callback changed the file.
    pub fn add(&mut self, path: &Path, addr: EndpointAddr) -> Result<(), PeersError> {
        let (current, ()) = Self::update(path, move |current| {
            if current.contains(&addr.id) {
                ((), false)
            } else {
                current.addrs.push(addr);
                ((), true)
            }
        })?;
        *self = current;
        Ok(())
    }

    /// Drops a peer from the trust list and persists it. Returns whether it
    /// was there — `false` lets a caller report "not a paired device" instead
    /// of silently succeeding on a typo'd id.
    pub fn remove(&mut self, path: &Path, id: &EndpointId) -> Result<bool, PeersError> {
        let id = *id;
        let (current, was_known) = Self::update(path, move |current| {
            let before = current.addrs.len();
            current.addrs.retain(|addr| addr.id != id);
            (before != current.addrs.len(), before != current.addrs.len())
        })?;
        *self = current;
        Ok(was_known)
    }

    /// Serializes a complete read-modify-write transaction with a distinct
    /// sibling lock file. This deliberately does not take `mesh.lock`: a
    /// running mesh must be able to observe a one-shot `kiem pair` update.
    fn update<T>(
        path: &Path,
        update: impl FnOnce(&mut Self) -> (T, bool),
    ) -> Result<(Self, T), PeersError> {
        let _lock = acquire_update_lock(path)?;
        let mut current = Self::load(path)?;
        let (result, changed) = update(&mut current);
        if changed {
            current.save_unlocked(path)?;
        }
        Ok((current, result))
    }

    /// Writes a completed replacement beside the old file, then atomically
    /// renames it into place. Readers therefore see either the prior complete
    /// list or the next complete list, never a truncated file.
    fn save_unlocked(&self, path: &Path) -> Result<(), PeersError> {
        let contents = self
            .addrs
            .iter()
            .map(|addr| EndpointTicket::new(addr.clone()).to_string())
            .collect::<Vec<_>>()
            .join("\n");
        atomic_write(path, contents.as_bytes()).map_err(|source| PeersError::Write {
            path: path.display().to_string(),
            source,
        })
    }
}

/// Acquires a short-lived advisory lock for a full known-peers transaction.
/// It is separate from `mesh.lock`: a mesh holds that lock for its lifetime,
/// while CLI pair/forget and first-contact callbacks must still update peers.
fn acquire_update_lock(path: &Path) -> Result<File, PeersError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    crate::storage::ensure_private_data_dir(parent).map_err(|source| PeersError::Lock {
        path: parent.display().to_string(),
        source,
    })?;

    let lock_path = path.with_file_name(format!(".{}.lock", file_name(path)));
    let mut options = File::options();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&lock_path)
        .map_err(|source| PeersError::Lock {
            path: lock_path.display().to_string(),
            source,
        })?;

    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
            Err(TryLockError::Error(source)) => {
                return Err(PeersError::Lock {
                    path: lock_path.display().to_string(),
                    source,
                });
            }
        }
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let (temp_path, mut temp_file) = create_temporary_file(path)?;
    if let Err(error) = temp_file
        .write_all(contents)
        .and_then(|()| temp_file.sync_all())
    {
        drop(temp_file);
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    drop(temp_file);

    match fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temp_path);
            Err(error)
        }
    }
}

fn create_temporary_file(path: &Path) -> std::io::Result<(PathBuf, File)> {
    static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

    for _ in 0..256 {
        let temp_path = path.with_file_name(format!(
            ".{}.tmp.{}.{}",
            file_name(path),
            std::process::id(),
            NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = File::options();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temp_path) {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique known-peers temporary file",
    ))
}

fn file_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("known-peers")
}

/// Generates a shareable ticket for this device, to be pasted or scanned (as a
/// QR code) on another device during pairing.
pub fn my_ticket(endpoint: &Endpoint) -> EndpointTicket {
    EndpointTicket::new(endpoint.addr())
}

/// Parses a pasted/scanned ticket into the peer's address, for both connecting
/// and recording in the known-peers store.
pub fn parse_ticket(ticket: &str) -> Result<EndpointAddr, PeersError> {
    // Tickets are one token; tolerate visual line wrapping when pasted from a UI.
    let ticket: EndpointTicket = ticket
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .parse()?;
    Ok(ticket.endpoint_addr().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_ticket_into_the_known_peers_store() {
        let dir = tempfile::tempdir().unwrap();
        let peers_path = dir.path().join("known-peers");

        let their_id = iroh::SecretKey::generate().public();
        let their_addr = EndpointAddr::from(their_id);
        let ticket = EndpointTicket::new(their_addr).to_string();

        let addr = parse_ticket(&ticket).unwrap();
        let mut peers = KnownPeers::load(&peers_path).unwrap();
        peers.add(&peers_path, addr).unwrap();

        let reloaded = KnownPeers::load(&peers_path).unwrap();
        assert!(reloaded.contains(&their_id));
    }

    #[cfg(unix)]
    #[test]
    fn known_peers_and_its_update_lock_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let peers_path = dir.path().join("known-peers");
        let addr = EndpointAddr::from(iroh::SecretKey::generate().public());
        let mut peers = KnownPeers::load(&peers_path).unwrap();
        peers.add(&peers_path, addr).unwrap();

        for path in [peers_path.clone(), dir.path().join(".known-peers.lock")] {
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600,
                "{} must not be group/other-readable",
                path.display()
            );
        }
    }

    #[test]
    fn stale_peer_lists_merge_competing_additions_instead_of_losing_one() {
        let dir = tempfile::tempdir().unwrap();
        let peers_path = dir.path().join("known-peers");
        let first_addr = EndpointAddr::from(iroh::SecretKey::generate().public());
        let second_addr = EndpointAddr::from(iroh::SecretKey::generate().public());
        let (first_id, second_id) = (first_addr.id, second_addr.id);

        // These represent two one-shot processes (or two first-contact
        // callbacks) that both observed the same initial file before either
        // persisted its addition.
        let mut first = KnownPeers::load(&peers_path).unwrap();
        let mut second = KnownPeers::load(&peers_path).unwrap();
        first.add(&peers_path, first_addr).unwrap();
        second.add(&peers_path, second_addr).unwrap();

        let persisted = KnownPeers::load(&peers_path).unwrap();
        assert!(persisted.contains(&first_id));
        assert!(persisted.contains(&second_id));
    }

    #[test]
    fn parses_a_visually_wrapped_ticket() {
        let their_id = iroh::SecretKey::generate().public();
        let ticket = EndpointTicket::new(EndpointAddr::from(their_id)).to_string();
        let middle = ticket.len() / 2;
        let wrapped = format!("  {}\n{}  ", &ticket[..middle], &ticket[middle..]);

        assert_eq!(parse_ticket(&wrapped).unwrap().id, their_id);
    }

    #[test]
    fn rejects_a_garbage_ticket_without_panicking() {
        assert!(parse_ticket("not a ticket").is_err());
    }

    #[test]
    fn removing_a_peer_persists_and_reports_whether_it_was_known() {
        let dir = tempfile::tempdir().unwrap();
        let peers_path = dir.path().join("known-peers");

        let (kept, dropped) = (
            EndpointAddr::from(iroh::SecretKey::generate().public()),
            EndpointAddr::from(iroh::SecretKey::generate().public()),
        );
        let (kept_id, dropped_id) = (kept.id, dropped.id);
        let mut peers = KnownPeers::load(&peers_path).unwrap();
        peers.add(&peers_path, kept).unwrap();
        peers.add(&peers_path, dropped).unwrap();

        assert!(peers.remove(&peers_path, &dropped_id).unwrap());
        assert!(
            !peers.remove(&peers_path, &dropped_id).unwrap(),
            "removing an unknown peer should report false, not succeed silently"
        );

        // The removal has to survive the process, not just this instance —
        // otherwise the peer is trusted again on the next launch.
        let reloaded = KnownPeers::load(&peers_path).unwrap();
        assert!(!reloaded.contains(&dropped_id));
        assert!(reloaded.contains(&kept_id), "removal took the wrong peer");
    }

    #[test]
    fn a_missing_peers_file_is_an_empty_list_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let peers = KnownPeers::load(&dir.path().join("does-not-exist")).unwrap();
        assert!(peers.ids().is_empty());
    }
}
