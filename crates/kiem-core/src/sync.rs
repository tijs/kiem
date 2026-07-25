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

/// Well-known doc id for the purge set (see `NoteStore::tombstone_doc_bytes`).
/// It rides the normal per-document sync, so purges propagate with zero
/// protocol changes; the underscore prefix keeps it clear of note UUIDs.
pub const TOMBSTONES_DOC_ID: &str = "_kiem/tombstones";

#[derive(Default)]
pub struct SyncEngine {
    /// (peer_id, doc_id) → sync state.
    states: HashMap<(String, String), State>,
    /// Documents received from peers that do not hydrate to a note yet.
    pending: HashMap<String, AutoCommit>,
    /// doc_id → (last-seen stored bytes, the `AutoCommit` parsed from them).
    /// `generate_message` runs once per doc per peer per tick regardless of
    /// whether anything changed, so on a store with hundreds of documents
    /// almost every call would otherwise reparse an unchanged document from
    /// scratch (`AutoCommit::load`, the expensive part) — this cache skips
    /// that. Staleness is decided by `stamps` (see below); the bytes kept
    /// here back the byte-compare `receive_message` still does on its
    /// fetched copy.
    loaded: HashMap<String, (Vec<u8>, AutoCommit)>,
    /// doc_id → the SQLite `modified_at` stamp observed when `loaded`'s
    /// parse for that doc was cached. Every store write path bumps
    /// `modified_at`, so `generate_message` can skip the per-doc BLOB
    /// `SELECT` entirely (the dominant cost of an idle tick over hundreds
    /// of docs) when the cheap stamp read still matches — including writes
    /// that land outside `SyncEngine` (a local edit via the CLI/app).
    stamps: HashMap<String, String>,
    /// Test seam: how many note-BLOB fetches `generate_message` has done.
    #[cfg(test)]
    note_blob_fetches: usize,
}

impl SyncEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every document id this engine should sync: everything in the store
    /// (trashed included), the tombstone/purge set, plus in-flight documents.
    pub fn doc_ids(&self, store: &NoteStore) -> Result<Vec<String>, SyncError> {
        let mut ids = store.list_all_ids()?;
        ids.push(TOMBSTONES_DOC_ID.to_owned());
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
        // Cheap staleness pre-check for regular docs: an unchanged
        // `modified_at` stamp proves the stored bytes are unchanged, so the
        // cached parse can be synced from directly — no BLOB `SELECT`.
        let fresh_stamp = if doc_id == TOMBSTONES_DOC_ID || self.pending.contains_key(doc_id) {
            None
        } else {
            store.get_modified_at(doc_id)?
        };
        if let Some(stamp) = &fresh_stamp {
            if self.stamps.get(doc_id) == Some(stamp) {
                if let Some((_, doc)) = self.loaded.get_mut(doc_id) {
                    let message = doc.sync().generate_sync_message(state);
                    return Ok(message.map(|m| m.encode()));
                }
            }
        }
        let stored_bytes = if doc_id == TOMBSTONES_DOC_ID {
            store.tombstone_doc_bytes()?
        } else {
            #[cfg(test)]
            {
                self.note_blob_fetches += 1;
            }
            store.get_doc_bytes(doc_id)?
        };
        let message = if let Some(doc) = self.pending.get_mut(doc_id) {
            doc.sync().generate_sync_message(state)
        } else if let Some(bytes) = stored_bytes {
            let stale = match self.loaded.get(doc_id) {
                Some((cached_bytes, _)) => cached_bytes != &bytes,
                None => true,
            };
            if stale {
                let doc = load_doc(doc_id, &bytes)?;
                self.loaded.insert(doc_id.to_owned(), (bytes, doc));
            }
            if let Some(stamp) = fresh_stamp {
                self.stamps.insert(doc_id.to_owned(), stamp);
            }
            self.loaded
                .get_mut(doc_id)
                .expect("just inserted or already cached above")
                .1
                .sync()
                .generate_sync_message(state)
        } else if doc_id == TOMBSTONES_DOC_ID {
            // The tombstone doc syncs even before any purge exists: an empty
            // document still yields an initial handshake message, which
            // guarantees a freshly-paired empty store says *something* first.
            // That matters mechanically — QUIC streams open lazily, so a
            // dialer with nothing to send would leave the acceptor parked in
            // accept_bi forever and nothing would ever sync (the empty-dialer
            // deadlock behind the intermittent "final state: []" test hangs).
            AutoCommit::new().sync().generate_sync_message(state)
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
        let stored_bytes = if doc_id == TOMBSTONES_DOC_ID {
            store.tombstone_doc_bytes()?
        } else {
            store.get_doc_bytes(doc_id)?
        };
        let mut doc = match self.pending.remove(doc_id) {
            Some(doc) => doc,
            None => match stored_bytes {
                // Reuse the cached parse when it's still current (the doc
                // hasn't changed since); the entry is removed either way —
                // this doc is about to be mutated, so any cached copy is
                // stale the moment we're done regardless of which branch we
                // took (see the `loaded` field doc comment).
                Some(bytes) => match self.loaded.remove(doc_id) {
                    Some((cached_bytes, doc)) if cached_bytes == bytes => doc,
                    _ => load_doc(doc_id, &bytes)?,
                },
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

        if doc_id == TOMBSTONES_DOC_ID {
            // The purge set always hydrates (an empty map is valid); adopting
            // it persists the merged doc and erases the listed notes locally.
            store.adopt_tombstone_doc(&mut doc)?;
        } else if hydrate::<_, NoteDoc>(&doc).is_ok() {
            // Deferred: a sync burst can apply many documents back to back;
            // the transport layer flushes the search index once per tick
            // (`flush_search_index`) rather than paying a commit per note.
            store.put_doc_deferred(&mut doc)?;
        } else {
            self.pending.insert(doc_id.to_owned(), doc);
        }
        Ok(())
    }

    /// Whether `doc_id` is parked mid-initial-sync (known id, not yet enough
    /// changes to hydrate a note). Diagnostic only — the transport uses it to
    /// tell "we have no reply because we're converged" apart from "we have no
    /// reply and the document is still incomplete", which look identical from
    /// outside but mean opposite things.
    pub fn is_pending(&self, doc_id: &str) -> bool {
        self.pending.contains_key(doc_id)
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
    /// to say. Returns the number of messages that crossed the wire. Flushes
    /// both sides' deferred search-index writes on convergence, mirroring
    /// what the real transport's per-tick `flush_search_index` call does in
    /// production (see session.rs's `sync_round`).
    fn converge(a: &mut Peer, b: &mut Peer) -> usize {
        let mut total = 0;
        loop {
            let round = pump(a, b) + pump(b, a);
            if round == 0 {
                a.store.flush_search_index().unwrap();
                b.store.flush_search_index().unwrap();
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
            .insert_note(&NoteDoc::new_with(
                "na".into(),
                "# From A",
                "did:a",
                TS.into(),
            ))
            .unwrap();
        b.store
            .insert_note(&NoteDoc::new_with(
                "nb".into(),
                "# From B",
                "did:b",
                TS.into(),
            ))
            .unwrap();

        converge(&mut a, &mut b);

        for p in [&a, &b] {
            let ids: Vec<String> = p
                .store
                .list_notes()
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .collect();
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

        let body_a = a
            .store
            .get_note("n1")
            .unwrap()
            .unwrap()
            .body
            .as_str()
            .to_owned();
        let body_b = b
            .store
            .get_note("n1")
            .unwrap()
            .unwrap()
            .body
            .as_str()
            .to_owned();
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

    /// One idle "tick": generate (and drop) messages for every doc, as the
    /// transport's `sync_round` does.
    fn tick(p: &mut Peer, to: &str) {
        for id in p.engine.doc_ids(&p.store).unwrap() {
            p.engine.generate_message(&p.store, to, &id).unwrap();
        }
    }

    #[test]
    fn unchanged_docs_skip_blob_fetches_after_first_tick() {
        let (mut a, mut b) = (peer("a"), peer("b"));
        for i in 0..10 {
            a.store
                .insert_note(&NoteDoc::new_with(
                    format!("n{i}"),
                    &format!("# Note {i}"),
                    "did:a",
                    TS.into(),
                ))
                .unwrap();
        }
        converge(&mut a, &mut b);

        // First idle tick may rebuild caches invalidated by the last
        // receive; every tick after that must be BLOB-free.
        tick(&mut a, "b");
        let after_first = a.engine.note_blob_fetches;
        for _ in 0..5 {
            tick(&mut a, "b");
        }
        assert_eq!(
            a.engine.note_blob_fetches, after_first,
            "idle ticks over unchanged docs must not fetch note BLOBs"
        );
    }

    #[test]
    fn edit_between_ticks_is_picked_up_on_the_next_tick() {
        let (mut a, mut b) = (peer("a"), peer("b"));
        a.store
            .insert_note(&NoteDoc::new_with("n1".into(), "# X", "did:a", TS.into()))
            .unwrap();
        converge(&mut a, &mut b);
        // Prime the watermark so the skip path is active, then write behind
        // the engine's back the way the CLI/app does.
        tick(&mut a, "b");
        tick(&mut a, "b");
        a.store.update_note("n1", "# X\n\nedited between ticks").unwrap();

        converge(&mut a, &mut b);

        assert!(b
            .store
            .get_note("n1")
            .unwrap()
            .unwrap()
            .body
            .as_str()
            .contains("edited between ticks"));
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
        assert!(
            b.store.search("Bye", 10).unwrap().is_empty(),
            "trashed note left B's index"
        );
    }

    #[test]
    fn interrupted_sync_resumes_with_fresh_state() {
        // A peer restart drops sync states; the next handshake must still
        // converge (states are an optimization, not a correctness carrier).
        let (mut a, mut b) = (peer("a"), peer("b"));
        a.store
            .insert_note(&NoteDoc::new_with(
                "n1".into(),
                "# Resume",
                "did:a",
                TS.into(),
            ))
            .unwrap();
        converge(&mut a, &mut b);

        b.engine = SyncEngine::new(); // B "restarted"
        a.engine.forget_peer("b");
        a.store
            .update_note("n1", "# Resume\n\nafter restart")
            .unwrap();
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

    /// Like `converge`, but bails out after `max_rounds` instead of looping
    /// forever — a livelock should fail the test, not hang the suite.
    fn converge_bounded(a: &mut Peer, b: &mut Peer, max_rounds: usize) -> Option<usize> {
        let mut total = 0;
        for _ in 0..max_rounds {
            let round = pump(a, b) + pump(b, a);
            if round == 0 {
                a.store.flush_search_index().unwrap();
                b.store.flush_search_index().unwrap();
                return Some(total);
            }
            total += round;
        }
        None
    }

    /// Reproduces a livelock reported live (finding baf2d005): two peers with
    /// ~600 already-converged notes both restart near-simultaneously — unlike
    /// `interrupted_sync_resumes_with_fresh_state`, which only resets *one*
    /// side, here *both* engines reset at once, so both sides need a fresh
    /// per-doc handshake simultaneously. Live symptom: continuous non-empty
    /// sync activity forever with zero notes ever actually crossing.
    #[test]
    fn both_peers_restarting_simultaneously_still_converges() {
        let (mut a, mut b) = (peer("a"), peer("b"));
        for i in 0..600 {
            a.store
                .insert_note(&NoteDoc::new_with(
                    format!("shared-{i}"),
                    &format!("# Note {i}"),
                    "did:a",
                    TS.into(),
                ))
                .unwrap();
        }
        converge_bounded(&mut a, &mut b, 10_000).expect("initial convergence of 600 notes");
        assert_eq!(b.store.list_notes().unwrap().len(), 600);

        // Both peers "restart" at once: both engines reset, unlike the
        // single-sided reset in interrupted_sync_resumes_with_fresh_state.
        a.engine = SyncEngine::new();
        b.engine = SyncEngine::new();

        a.store
            .insert_note(&NoteDoc::new_with(
                "new-from-a".into(),
                "# New from A",
                "did:a",
                TS.into(),
            ))
            .unwrap();
        b.store
            .insert_note(&NoteDoc::new_with(
                "new-from-b".into(),
                "# New from B",
                "did:b",
                TS.into(),
            ))
            .unwrap();

        let result = converge_bounded(&mut a, &mut b, 10_000);
        assert!(
            result.is_some(),
            "did not converge within 10,000 rounds after a simultaneous double restart"
        );

        assert!(
            a.store.get_note("new-from-b").unwrap().is_some(),
            "A never got B's new note"
        );
        assert!(
            b.store.get_note("new-from-a").unwrap().is_some(),
            "B never got A's new note"
        );
    }
}
