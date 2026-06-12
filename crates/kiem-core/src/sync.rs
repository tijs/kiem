//! Transport-agnostic Automerge sync engine.
//!
//! [`SyncEngine`] holds per-(peer, document) `automerge::sync::State` and
//! turns store contents into sync messages and incoming messages into store
//! writes. Bytes in, bytes out — TCP, NWConnection, or an in-process channel
//! all look the same from here.
//!
//! Documents mid-initial-sync (known id, not yet enough changes to hydrate a
//! valid note) are parked in `pending` rather than persisted; they move into
//! the store the moment they hydrate. Sync states live in memory only — a
//! restarted peer just pays a full (cheap) re-handshake.

use std::collections::HashMap;

use automerge::sync::{Message, State, SyncDoc};
use automerge::AutoCommit;
use autosurgeon::hydrate;

use crate::note::NoteDoc;
use crate::store::{NoteStore, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("sync message decode error: {0}")]
    Decode(String),
    #[error("sync protocol error for document {doc_id}: {message}")]
    Protocol { doc_id: String, message: String },
}

#[derive(Default)]
pub struct SyncEngine {
    /// (peer_id, doc_id) → sync state.
    states: HashMap<(String, String), State>,
    /// Documents received from peers that do not hydrate to a note yet.
    pending: HashMap<String, AutoCommit>,
}

impl SyncEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every document id this engine should sync: everything in the store
    /// (trashed included) plus in-flight documents.
    pub fn doc_ids(&self, store: &NoteStore) -> Result<Vec<String>, SyncError> {
        let mut ids = store.list_all_ids()?;
        for id in self.pending.keys() {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        Ok(ids)
    }

    /// Next sync message for `peer` about `doc_id`, if any. `None` means
    /// converged (nothing to say right now).
    pub fn generate_message(
        &mut self,
        store: &NoteStore,
        peer: &str,
        doc_id: &str,
    ) -> Result<Option<Vec<u8>>, SyncError> {
        // Direct field access keeps the `states` and `pending` borrows
        // disjoint for the borrow checker.
        let state = self
            .states
            .entry((peer.to_owned(), doc_id.to_owned()))
            .or_default();
        let message = if let Some(doc) = self.pending.get_mut(doc_id) {
            doc.sync().generate_sync_message(state)
        } else if let Some(bytes) = store.get_doc_bytes(doc_id)? {
            load_doc(doc_id, &bytes)?.sync().generate_sync_message(state)
        } else {
            // Unknown doc: nothing to offer (the peer's message will
            // introduce it through receive_message).
            None
        };
        Ok(message.map(|m| m.encode()))
    }

    /// Apply an incoming sync message. Persists the document if it now
    /// hydrates to a valid note; parks it as pending otherwise.
    pub fn receive_message(
        &mut self,
        store: &mut NoteStore,
        peer: &str,
        doc_id: &str,
        bytes: &[u8],
    ) -> Result<(), SyncError> {
        let message = Message::decode(bytes).map_err(|e| SyncError::Decode(e.to_string()))?;
        let mut doc = match self.pending.remove(doc_id) {
            Some(doc) => doc,
            None => match store.get_doc_bytes(doc_id)? {
                Some(bytes) => load_doc(doc_id, &bytes)?,
                None => AutoCommit::new(),
            },
        };
        let state = self.state_for(peer, doc_id);
        doc.sync()
            .receive_sync_message(state, message)
            .map_err(|e| SyncError::Protocol {
                doc_id: doc_id.to_owned(),
                message: e.to_string(),
            })?;

        if hydrate::<_, NoteDoc>(&doc).is_ok() {
            store.put_doc(&mut doc)?;
        } else {
            self.pending.insert(doc_id.to_owned(), doc);
        }
        Ok(())
    }

    /// Drop all sync state for a peer (it reconnects with a fresh handshake).
    pub fn forget_peer(&mut self, peer: &str) {
        self.states.retain(|(p, _), _| p != peer);
    }

    fn state_for(&mut self, peer: &str, doc_id: &str) -> &mut State {
        self.states
            .entry((peer.to_owned(), doc_id.to_owned()))
            .or_default()
    }
}

fn load_doc(doc_id: &str, bytes: &[u8]) -> Result<AutoCommit, SyncError> {
    AutoCommit::load(bytes).map_err(|e| SyncError::Protocol {
        doc_id: doc_id.to_owned(),
        message: format!("stored document failed to load: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::NoteDoc;

    const TS: &str = "2026-06-12T10:00:00Z";

    struct Peer {
        name: &'static str,
        store: NoteStore,
        engine: SyncEngine,
    }

    fn peer(name: &'static str) -> Peer {
        Peer {
            name,
            store: NoteStore::open_in_memory_with_search().unwrap(),
            engine: SyncEngine::new(),
        }
    }

    /// Pump every pending message from one peer to the other; returns count.
    fn pump(from: &mut Peer, to: &mut Peer) -> usize {
        let mut sent = 0;
        for id in from.engine.doc_ids(&from.store).unwrap() {
            if let Some(msg) = from
                .engine
                .generate_message(&from.store, to.name, &id)
                .unwrap()
            {
                to.engine
                    .receive_message(&mut to.store, from.name, &id, &msg)
                    .unwrap();
                sent += 1;
            }
        }
        sent
    }

    /// Exchange messages in both directions until neither side has anything
    /// to say. Returns the number of messages that crossed the wire.
    fn converge(a: &mut Peer, b: &mut Peer) -> usize {
        let mut total = 0;
        loop {
            let round = pump(a, b) + pump(b, a);
            if round == 0 {
                return total;
            }
            total += round;
        }
    }

    #[test]
    fn note_created_on_one_peer_appears_on_the_other() {
        let (mut a, mut b) = (peer("a"), peer("b"));
        let note = NoteDoc::new_with("n1".into(), "# Hello\n\nfrom A #sync", "did:a", TS.into());
        a.store.insert_note(&note).unwrap();

        converge(&mut a, &mut b);

        let got = b.store.get_note("n1").unwrap().expect("synced to B");
        assert_eq!(got.body.as_str(), "# Hello\n\nfrom A #sync");
        assert_eq!(got.metadata.title, "Hello");
        // B's denormalized columns and search index follow the synced doc.
        assert_eq!(b.store.list_by_tag("sync").unwrap().len(), 1);
        assert_eq!(b.store.search("Hello", 10).unwrap().len(), 1);
    }

    #[test]
    fn peers_with_independent_notes_end_up_with_all_of_them() {
        let (mut a, mut b) = (peer("a"), peer("b"));
        a.store
            .insert_note(&NoteDoc::new_with("na".into(), "# From A", "did:a", TS.into()))
            .unwrap();
        b.store
            .insert_note(&NoteDoc::new_with("nb".into(), "# From B", "did:b", TS.into()))
            .unwrap();

        converge(&mut a, &mut b);

        for p in [&a, &b] {
            let ids: Vec<String> = p.store.list_notes().unwrap().into_iter().map(|m| m.id).collect();
            assert_eq!(ids.len(), 2, "{} should have both notes", p.name);
        }
    }

    #[test]
    fn concurrent_edits_to_the_same_note_merge_on_both_sides() {
        let (mut a, mut b) = (peer("a"), peer("b"));
        let note = NoteDoc::new_with("n1".into(), "# Shared\n\nbase", "did:a", TS.into());
        a.store.insert_note(&note).unwrap();
        converge(&mut a, &mut b);

        // Diverge: A appends one line, B appends a different one.
        a.store
            .update_note("n1", "# Shared\n\nbase\nline from A")
            .unwrap();
        b.store
            .update_note("n1", "# Shared\n\nbase\nline from B")
            .unwrap();
        converge(&mut a, &mut b);

        let body_a = a.store.get_note("n1").unwrap().unwrap().body.as_str().to_owned();
        let body_b = b.store.get_note("n1").unwrap().unwrap().body.as_str().to_owned();
        assert_eq!(body_a, body_b, "peers must converge to identical bodies");
        assert!(body_a.contains("line from A") && body_a.contains("line from B"));
    }

    #[test]
    fn converged_peers_exchange_minimal_traffic() {
        let (mut a, mut b) = (peer("a"), peer("b"));
        a.store
            .insert_note(&NoteDoc::new_with("n1".into(), "# X", "did:a", TS.into()))
            .unwrap();
        converge(&mut a, &mut b);
        // Re-running produces no document traffic beyond (at most) one
        // already-in-sync handshake message per direction per doc.
        let second = converge(&mut a, &mut b);
        assert!(second <= 2, "expected near-zero traffic, got {second}");
    }

    #[test]
    fn soft_delete_propagates() {
        let (mut a, mut b) = (peer("a"), peer("b"));
        a.store
            .insert_note(&NoteDoc::new_with("n1".into(), "# Bye", "did:a", TS.into()))
            .unwrap();
        converge(&mut a, &mut b);
        a.store.delete_note("n1").unwrap();
        converge(&mut a, &mut b);

        assert!(b.store.list_notes().unwrap().is_empty());
        assert_eq!(b.store.list_deleted().unwrap().len(), 1);
        assert!(b.store.search("Bye", 10).unwrap().is_empty(), "trashed note left B's index");
    }

    #[test]
    fn interrupted_sync_resumes_with_fresh_state() {
        // A peer restart drops sync states; the next handshake must still
        // converge (states are an optimization, not a correctness carrier).
        let (mut a, mut b) = (peer("a"), peer("b"));
        a.store
            .insert_note(&NoteDoc::new_with("n1".into(), "# Resume", "did:a", TS.into()))
            .unwrap();
        converge(&mut a, &mut b);

        b.engine = SyncEngine::new(); // B "restarted"
        a.engine.forget_peer("b");
        a.store.update_note("n1", "# Resume\n\nafter restart").unwrap();
        converge(&mut a, &mut b);

        assert!(b
            .store
            .get_note("n1")
            .unwrap()
            .unwrap()
            .body
            .as_str()
            .contains("after restart"));
    }
}
