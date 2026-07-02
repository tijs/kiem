use std::path::Path;

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
    #[error("known-peers file at {path} has an invalid endpoint id on line {line}")]
    Corrupt { path: String, line: usize },
    #[error("invalid ticket: {0}")]
    InvalidTicket(#[from] iroh_tickets::ParseError),
}

/// A device's trust list, replacing mDNS's "any peer on the LAN" auto-discovery
/// with an explicit, one-time pairing step. Persisted as one `EndpointId` per
/// line — human-readable, diffable, no format ceremony for a handful of entries.
pub struct KnownPeers {
    ids: Vec<EndpointId>,
}

impl KnownPeers {
    pub fn load(path: &Path) -> Result<Self, PeersError> {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let mut ids = Vec::new();
                for (line_no, line) in contents.lines().enumerate() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let id: EndpointId = line.parse().map_err(|_| PeersError::Corrupt {
                        path: path.display().to_string(),
                        line: line_no + 1,
                    })?;
                    ids.push(id);
                }
                Ok(Self { ids })
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self { ids: Vec::new() })
            }
            Err(source) => Err(PeersError::Read {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    pub fn contains(&self, id: &EndpointId) -> bool {
        self.ids.contains(id)
    }

    pub fn ids(&self) -> &[EndpointId] {
        &self.ids
    }

    /// Adds a peer (no-op if already known) and persists the updated list.
    pub fn add(&mut self, path: &Path, id: EndpointId) -> Result<(), PeersError> {
        if self.contains(&id) {
            return Ok(());
        }
        self.ids.push(id);
        self.save(path)
    }

    fn save(&self, path: &Path) -> Result<(), PeersError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| PeersError::Write {
                path: path.display().to_string(),
                source,
            })?;
        }
        let contents = self
            .ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(path, contents).map_err(|source| PeersError::Write {
            path: path.display().to_string(),
            source,
        })
    }
}

/// Generates a shareable ticket for this device, to be pasted or scanned (as a
/// QR code) on another device during pairing.
pub fn my_ticket(endpoint: &Endpoint) -> EndpointTicket {
    EndpointTicket::new(endpoint.addr())
}

/// Parses a pasted/scanned ticket into the peer's address, for both connecting
/// and recording in the known-peers store.
pub fn parse_ticket(ticket: &str) -> Result<EndpointAddr, PeersError> {
    let ticket: EndpointTicket = ticket.parse()?;
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
        peers.add(&peers_path, addr.id).unwrap();

        let reloaded = KnownPeers::load(&peers_path).unwrap();
        assert!(reloaded.contains(&their_id));
    }

    #[test]
    fn rejects_a_garbage_ticket_without_panicking() {
        assert!(parse_ticket("not a ticket").is_err());
    }

    #[test]
    fn a_missing_peers_file_is_an_empty_list_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let peers = KnownPeers::load(&dir.path().join("does-not-exist")).unwrap();
        assert!(peers.ids().is_empty());
    }
}
