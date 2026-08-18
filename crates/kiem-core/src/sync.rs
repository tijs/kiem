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
//! process restart pays a full per-document handshake, which on a store with
//! hundreds of notes is not cheap; a mere *disconnect* keeps the reusable part
//! (see [`SyncEngine::reset_peer`]).

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
    /// How many note-BLOB fetches `generate_message` has done — see
    /// [`SyncEngine::note_blob_fetches`].
    note_blob_fetches: usize,
}

/// `KIEM_SYNC_TRACE_DOC=<id-prefix>` prints one stderr line per sync-engine
/// call touching a matching document — the per-document microscope for "why
/// does this one doc not converge" (finding baf2d005). Off by default;
/// kiem-sync's `KIEM_SYNC_TRACE` is the round-level view, this is doc-level.
fn traced(doc_id: &str) -> bool {
    static FILTER: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    FILTER
        .get_or_init(|| {
            std::env::var("KIEM_SYNC_TRACE_DOC")
                .ok()
                .filter(|v| !v.is_empty())
        })
        .as_ref()
        .is_some_and(|prefix| doc_id.starts_with(prefix.as_str()))
}

/// Compact shape of a sync message for `traced` output.
fn message_shape(message: &Message) -> String {
    format!(
        "heads={} need={} have={} changes={}",
        message.heads.len(),
        message.need.len(),
        message.have.len(),
        message.changes.len()
    )
}

fn trace_gen(peer: &str, doc_id: &str, branch: &str, message: &Option<Message>) {
    let out = match message {
        Some(m) => format!("[{}]", message_shape(m)),
        None => "none".to_owned(),
    };
    eprintln!(
        "kiem sync doc-trace: gen peer={} doc={doc_id} branch={branch} out={out}",
        peer.get(..8).unwrap_or(peer)
    );
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
        let branch;
        if let Some(stamp) = &fresh_stamp {
            if self.stamps.get(doc_id) == Some(stamp) {
                if let Some((_, doc)) = self.loaded.get_mut(doc_id) {
                    let message = doc.sync().generate_sync_message(state);
                    if traced(doc_id) {
                        trace_gen(peer, doc_id, "cached-skip", &message);
                    }
                    return Ok(message.map(|m| m.encode()));
                }
            }
        }
        let stored_bytes = if doc_id == TOMBSTONES_DOC_ID {
            store.tombstone_doc_bytes()?
        } else {
            self.note_blob_fetches += 1;
            store.get_doc_bytes(doc_id)?
        };
        let message = if let Some(doc) = self.pending.get_mut(doc_id) {
            branch = "pending";
            doc.sync().generate_sync_message(state)
        } else if let Some(bytes) = stored_bytes {
            let stale = match self.loaded.get(doc_id) {
                Some((cached_bytes, _)) => cached_bytes != &bytes,
                None => true,
            };
            branch = if stale {
                "stored-reload"
            } else {
                "stored-cached"
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
            branch = "tombstone-empty";
            // The tombstone doc syncs even before any purge exists: an empty
            // document still yields an initial handshake message, which
            // guarantees a freshly-paired empty store says *something* first.
            // That matters mechanically — QUIC streams open lazily, so a
            // dialer with nothing to send would leave the acceptor parked in
            // accept_bi forever and nothing would ever sync (the empty-dialer
            // deadlock behind the intermittent "final state: []" test hangs).
            AutoCommit::new().sync().generate_sync_message(state)
        } else {
            branch = "unknown";
            // Unknown doc: nothing to offer (the peer's message will
            // introduce it through receive_message).
            None
        };
        if traced(doc_id) {
            trace_gen(peer, doc_id, branch, &message);
        }
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
        let in_shape = traced(doc_id).then(|| message_shape(&message));
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

        let outcome;
        if doc_id == TOMBSTONES_DOC_ID {
            // The purge set always hydrates (an empty map is valid); adopting
            // it persists the merged doc and erases the listed notes locally.
            outcome = "tombstone";
            store.adopt_tombstone_doc(&mut doc)?;
        } else if hydrate::<_, NoteDoc>(&doc).is_ok() {
            // Deferred: a sync burst can apply many documents back to back;
            // the transport layer flushes the search index once per tick
            // (`flush_search_index`) rather than paying a commit per note.
            outcome = "stored";
            // `put_doc_deferred` re-reads and merges against the current SQLite
            // BLOB under a compare-and-swap predicate. The snapshot above can
            // be stale because a GUI/CLI process may edit this note while this
            // sync message is being decoded and applied.
            store.put_doc_deferred(&mut doc)?;
        } else {
            outcome = "pending";
            self.pending.insert(doc_id.to_owned(), doc);
        }
        if let Some(shape) = in_shape {
            eprintln!(
                "kiem sync doc-trace: recv peer={} doc={doc_id} in=[{shape}] outcome={outcome}",
                peer.get(..8).unwrap_or(peer)
            );
        }
        Ok(())
    }

    /// How many times [`generate_message`] has read a note's document BLOB
    /// from the store since this engine was created.
    ///
    /// Diagnostic. An *idle* round over unchanged documents should not move
    /// this at all — it is the counter for the regression that made a tick
    /// over ~680 notes cost 224ms instead of 11ms, where every document was
    /// re-read from disk just to discover it had not changed.
    ///
    /// [`generate_message`]: Self::generate_message
    pub fn note_blob_fetches(&self) -> usize {
        self.note_blob_fetches
    }

    /// Whether `doc_id` is parked mid-initial-sync (known id, not yet enough
    /// changes to hydrate a note). Diagnostic only — the transport uses it to
    /// tell "we have no reply because we're converged" apart from "we have no
    /// reply and the document is still incomplete", which look identical from
    /// outside but mean opposite things.
    pub fn is_pending(&self, doc_id: &str) -> bool {
        self.pending.contains_key(doc_id)
    }

    /// Roll every per-document sync state for `peer` back to the part that
    /// survives a connection: the shared heads. Session-scoped fields (what
    /// they last said they had/needed, what we already sent, in-flight) are
    /// cleared, so the next connection re-handshakes — but from the last point
    /// both sides provably agreed on, not from nothing.
    ///
    /// This used to drop the states outright, on the reasoning that a
    /// reconnect "just pays a cheap re-handshake". At 600+ documents it is not
    /// cheap: with empty shared heads every document's opening message carries
    /// a Bloom filter summarising that document's *entire* change graph, and
    /// both peers recompute the full set of hashes to send.
    ///
    /// `State::encode` is defined as encoding exactly the state that should be
    /// reused across connections, so the roundtrip is automerge's own answer to
    /// "what survives a disconnect" rather than ours. If the peer really did
    /// lose its document, it detects that our shared heads are unknown to it
    /// and replies with a `SYNC_RESET` message, which starts over.
    ///
    /// Keeping the entries means a disconnect never removes them: a
    /// `(peer, doc)` state lives as long as the peer stays paired, which is
    /// the whole point. Unpairing is what drops them — see [`forget_peer`],
    /// which every unpair path must call.
    ///
    /// [`forget_peer`]: Self::forget_peer
    pub fn reset_peer(&mut self, peer: &str) {
        for ((p, _), state) in self.states.iter_mut() {
            if p == peer {
                *state = State::decode(&state.encode()).unwrap_or_default();
            }
        }
    }

    /// Drop every per-document sync state for `peer` — unpairing, not
    /// disconnecting. The next contact (if the device is ever paired again)
    /// starts from nothing, exactly as if it had never been seen.
    ///
    /// The opposite of [`reset_peer`], which keeps the shared heads precisely
    /// so a reconnect is cheap. Removal is what bounds the map: a long-lived
    /// process (the headless daemon runs for weeks) would otherwise hold a
    /// discarded device's states until restart.
    ///
    /// [`reset_peer`]: Self::reset_peer
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
