//! Drives one iroh connection end-to-end: opens the single bidirectional
//! stream this protocol multiplexes everything over, then runs a reader loop
//! (apply incoming, reply per document) alongside a ticker (periodic full
//! sync round — also picks up local edits from other processes) until the
//! connection closes.
//!
//! This ports `kiem-cli`'s former TCP daemon loop onto iroh: same framing
//! (see [`write_frame`]), same [`SyncEngine`] semantics, async instead of
//! OS threads. iroh authenticates the peer's `EndpointId` as part of the
//! connection handshake, so — unlike the TCP version — there's no need for a
//! hello frame to identify who's on the other end.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use iroh::endpoint::{
    Connection, ConnectionError, ReadExactError, RecvStream, SendStream, WriteError,
};
use iroh::{EndpointAddr, EndpointId};
use kiem_core::store::NoteStore;
use kiem_core::sync::{SyncEngine, SyncError};

use crate::peers;

/// Note ids are UUIDs today; leave generous headroom.
const MAX_DOC_ID_LEN: u32 = 1024;
/// A full document snapshot travels inside one sync message.
const MAX_PAYLOAD_LEN: u32 = 64 * 1024 * 1024;

/// Reserved frame marker for the pairing prelude — each side's first frame
/// carries its own `EndpointTicket` under this id so trust reciprocates in a
/// single connection. The `_kiem/` prefix keeps it clear of note UUIDs.
const PAIRING_HELLO: &str = "_kiem/pair";
const NAME_HELLO: &str = "_kiem/name";

/// Per-connection pairing handshake. `local_ticket` is this device's shareable
/// ticket (sent first thing on every connection); `on_peer` records the peer's
/// address into the trust list once, after it's been checked against the
/// iroh-authenticated peer id. `local_name` is a human-readable device name,
/// and `on_name` stores the name we learn from the peer. One connect ⇒ both
/// sides trust each other.
#[derive(Clone)]
pub struct PeerHandshake {
    pub local_ticket: String,
    pub local_name: String,
    pub on_peer: Arc<dyn Fn(EndpointAddr) + Send + Sync>,
    pub on_name: Arc<dyn Fn(EndpointId, String) + Send + Sync>,
    pub on_sync_activity: Arc<dyn Fn(EndpointId) + Send + Sync>,
}

#[derive(thiserror::Error, Debug)]
pub enum SessionError {
    #[error("connection error: {0}")]
    Connection(#[from] ConnectionError),
    #[error("stream write error: {0}")]
    Write(#[from] WriteError),
    #[error("stream read error: {0}")]
    Read(#[from] ReadExactError),
    /// Length limits guard against garbage on the wire — a bad frame is an
    /// error, not an allocation.
    #[error("frame field too large: {field} is {len} bytes (max {max})")]
    Oversized {
        field: &'static str,
        len: u32,
        max: u32,
    },
    #[error("doc id is not valid UTF-8")]
    BadDocId,
    #[error("sync error: {0}")]
    Sync(#[from] SyncError),
}

/// Shared, lockable note store + sync engine — the same pairing
/// `kiem-cli`'s daemon holds today, just behind an `Arc` for async tasks.
pub type SharedState = Arc<Mutex<(NoteStore, SyncEngine)>>;

/// Runs one peer session until the connection closes. `dialed` picks which
/// side opens the bidirectional stream (mirrors QUIC's client/server roles;
/// exactly one side must open while the other accepts).
pub async fn run(
    connection: Connection,
    dialed: bool,
    state: SharedState,
    interval: Duration,
    handshake: PeerHandshake,
) -> Result<(), SessionError> {
    let peer_id = connection.remote_id();
    let peer = peer_id.to_string();
    let (mut send, mut recv) = if dialed {
        connection.open_bi().await?
    } else {
        connection.accept_bi().await?
    };

    // Pairing prelude: send our own ticket and name, then read the peer's ticket.
    // The dialer's first write is also what opens the lazily-created QUIC
    // stream and lets the acceptor's accept_bi complete. We record the peer's
    // address only if the ticket's id matches the id iroh already authenticated
    // on this connection — so a peer can't push someone else's address into our
    // trust list. The name frame is read by the normal reader loop and routed
    // to `on_name` so older peers that don't send one are harmless.
    write_frame(&mut send, PAIRING_HELLO, handshake.local_ticket.as_bytes()).await?;
    write_frame(&mut send, NAME_HELLO, handshake.local_name.as_bytes()).await?;

    let (marker, payload) = read_frame(&mut recv).await?;
    if marker == PAIRING_HELLO {
        if let Ok(addr) = peers::parse_ticket(&String::from_utf8_lossy(&payload)) {
            if addr.id == peer_id {
                (handshake.on_peer)(addr);
            }
        }
    }


    let send = Arc::new(tokio::sync::Mutex::new(send));

    let ticker = tokio::spawn(ticker_loop(
        state.clone(),
        peer_id,
        send.clone(),
        interval,
        handshake.on_sync_activity.clone(),
    ));

    let result = reader_loop(&mut recv, &state, peer_id, &send, &handshake).await;

    state.lock().unwrap().1.forget_peer(&peer);
    ticker.abort();
    result
}

async fn ticker_loop(
    state: SharedState,
    peer_id: EndpointId,
    send: Arc<tokio::sync::Mutex<SendStream>>,
    interval: Duration,
    on_activity: Arc<dyn Fn(EndpointId) + Send + Sync>,
) {
    // First round immediately: on the dialed side this is what actually
    // transmits the lazily-opened QUIC stream, letting the acceptor's
    // accept_bi complete (and it cuts first-sync latency by one interval).
    loop {
        if sync_round(&state, peer_id, &send, &*on_activity).await.is_err() {
            return;
        }
        tokio::time::sleep(interval).await;
    }
}

/// `KIEM_SYNC_TRACE=1` prints one line per sync round and per batch of
/// received frames. Off by default. This exists because a stalled session on
/// real hardware (high-latency link, hundreds of documents, two peers
/// restarting at once) is not reproducible over loopback — the numbers have
/// to come off the machines that stall. Run `KIEM_SYNC_TRACE=1 kiem sync` on
/// both sides to collect them.
pub fn trace_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("KIEM_SYNC_TRACE").is_ok_and(|v| v != "0"))
}

/// Short, greppable peer id for trace lines.
fn short(peer_id: EndpointId) -> String {
    peer_id.to_string().chars().take(8).collect()
}

/// One sync round: offer every known document to the peer.
async fn sync_round(
    state: &SharedState,
    peer_id: EndpointId,
    send: &tokio::sync::Mutex<SendStream>,
    on_activity: &(dyn Fn(EndpointId) + Send + Sync),
) -> Result<(), SessionError> {
    let peer = peer_id.to_string();
    let started = Instant::now();
    let doc_count;
    let frames = {
        // Timed separately: waiting here means another thread (a UI/CLI call,
        // or this connection's reader) held the store, which is a different
        // problem from the round itself being slow.
        let mut guard = state.lock().unwrap();
        let waited = started.elapsed();
        let (store, engine) = &mut *guard;
        // One problematic document (corrupt bytes, a protocol edge case)
        // must never take the whole round down with it — `ticker_loop`
        // returns for good on any `Err` from here, permanently breaking
        // sync with this peer over a single doc. Log and skip instead; a
        // doc that keeps failing just keeps getting logged and retried,
        // every other doc still syncs normally.
        let doc_ids = engine.doc_ids(store).unwrap_or_else(|e| {
            eprintln!("kiem sync: doc_ids failed, skipping this round: {e}");
            Vec::new()
        });
        doc_count = doc_ids.len();
        let mut frames = Vec::new();
        for doc_id in doc_ids {
            match engine.generate_message(store, &peer, &doc_id) {
                Ok(Some(payload)) => frames.push((doc_id, payload)),
                Ok(None) => {}
                Err(e) => {
                    eprintln!("kiem sync: generate_message failed for {doc_id}, skipping: {e}");
                }
            }
        }
        // Commits whatever `receive_message` deferred since the last tick
        // (see `NoteStore::put_doc_deferred`) — bounds search staleness to
        // about one tick instead of paying a full index commit per document
        // received during a sync burst. Best-effort: a search-index hiccup
        // (e.g. transient writer-lock contention with a concurrent CLI
        // command) must never kill the ticker the way a real connection
        // error should — `ticker_loop` returns for good on any `Err` here,
        // and notes still sync correctly either way (the index is a derived,
        // rebuildable structure, not sync-critical). Just retry next tick.
        if let Err(e) = store.flush_search_index() {
            eprintln!("kiem sync: search index flush failed, will retry next tick: {e}");
        }
        if trace_enabled() {
            eprintln!(
                "kiem sync trace: round peer={} docs={doc_count} frames={} bytes={} \
                 lock_wait={:?} build={:?}",
                short(peer_id),
                frames.len(),
                frames.iter().map(|(_, p)| p.len()).sum::<usize>(),
                waited,
                started.elapsed() - waited,
            );
        }
        frames
    };
    if frames.is_empty() {
        return Ok(());
    }
    let frame_count = frames.len();
    let write_started = Instant::now();
    let mut send = send.lock().await;
    let send_waited = write_started.elapsed();
    for (doc_id, payload) in frames {
        write_frame(&mut send, &doc_id, &payload).await?;
    }
    if trace_enabled() {
        eprintln!(
            "kiem sync trace: sent peer={} frames={frame_count} send_lock_wait={send_waited:?} \
             write={:?}",
            short(peer_id),
            write_started.elapsed() - send_waited,
        );
    }
    on_activity(peer_id);
    Ok(())
}

async fn reader_loop(
    recv: &mut RecvStream,
    state: &SharedState,
    peer_id: EndpointId,
    send: &tokio::sync::Mutex<SendStream>,
    handshake: &PeerHandshake,
) -> Result<(), SessionError> {
    let peer = peer_id.to_string();
    // Trace counters: a stalled session looks completely different depending
    // on whether frames stop arriving, arrive but don't apply, or apply but
    // produce no reply. Summarised once a second so a busy session doesn't
    // drown the log.
    let (mut frames_in, mut replies_out, mut applied_for) = (0u64, 0u64, Duration::ZERO);
    // A frame that produces no reply is the interesting case: it leaves the
    // peer's sync state unadvanced, so the peer re-offers the same document
    // next round. `no_doc` counts the ones where the store held nothing for
    // that id at all (purged, or never stored) — a different problem from a
    // genuinely converged document.
    let (mut silent_no_doc, mut silent_converged, mut silent_pending) = (0u64, 0u64, 0u64);
    let mut sample_no_doc = String::new();
    // Distinct documents seen in the reporting window. If this tracks
    // `frames_in`, the peer is cycling its whole store; if it is far smaller,
    // a handful of documents are ping-ponging.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut last_report = Instant::now();
    loop {
        let (doc_id, payload) = read_frame(recv).await?;
        if trace_enabled() {
            frames_in += 1;
            seen.insert(doc_id.clone());
            if last_report.elapsed() >= Duration::from_secs(1) {
                let mut samples: Vec<&str> = seen.iter().take(3).map(String::as_str).collect();
                samples.sort_unstable();
                eprintln!(
                    "kiem sync trace: recv peer={} frames_in={frames_in} distinct={} \
                     replies_out={replies_out} silent_no_doc={silent_no_doc} \
                     silent_converged={silent_converged} silent_pending={silent_pending} \
                     sample_no_doc={sample_no_doc} \
                     samples={samples:?} apply_time={applied_for:?}",
                    short(peer_id),
                    seen.len(),
                );
                last_report = Instant::now();
                (frames_in, replies_out, applied_for) = (0, 0, Duration::ZERO);
                (silent_no_doc, silent_converged, silent_pending) = (0, 0, 0);
                sample_no_doc.clear();
                seen.clear();
            }
        }
        if doc_id == NAME_HELLO {
            let name = String::from_utf8_lossy(&payload).into_owned();
            (handshake.on_name)(peer_id, name);
            continue;
        }
        // A single malformed/unexpected document must not tear down the
        // whole connection — that's what `?` here would do (the caller
        // aborts the ticker and forgets the peer on any `Err`). Log and
        // move on to the next frame; every other document still syncs.
        let apply_started = Instant::now();
        let mut silent_kind = 0u8; // 0 = stored/converged, 1 = no doc, 2 = pending
        let reply = {
            let (store, engine) = &mut *state.lock().unwrap();
            if let Err(e) = engine.receive_message(store, &peer, &doc_id, &payload) {
                eprintln!("kiem sync: receive_message failed for {doc_id}, skipping: {e}");
                None
            } else {
                let reply = match engine.generate_message(store, &peer, &doc_id) {
                    Ok(reply) => reply,
                    Err(e) => {
                        eprintln!("kiem sync: generate_message (reply) failed for {doc_id}, skipping: {e}");
                        None
                    }
                };
                if trace_enabled() && reply.is_none() {
                    silent_kind = if engine.is_pending(&doc_id) {
                        2
                    } else if store.get_doc_bytes(&doc_id).ok().flatten().is_some() {
                        0
                    } else {
                        1
                    };
                }
                reply
            }
        };
        applied_for += apply_started.elapsed();
        if let Some(reply_payload) = reply {
            replies_out += 1;
            let mut send = send.lock().await;
            write_frame(&mut send, &doc_id, &reply_payload).await?;
        } else if trace_enabled() {
            match silent_kind {
                2 => silent_pending += 1,
                0 => silent_converged += 1,
                _ => {
                    silent_no_doc += 1;
                    if sample_no_doc.is_empty() {
                        sample_no_doc = doc_id.clone();
                    }
                }
            }
        }
        (handshake.on_sync_activity)(peer_id);
    }
}

/// Wire format: `[doc_id_len][doc_id bytes][payload_len][payload bytes]`, all
/// lengths big-endian u32. No control frames — iroh's handshake already
/// authenticates the peer id. (Same framing the pre-iroh TCP daemon used.)
async fn write_frame(
    send: &mut SendStream,
    doc_id: &str,
    payload: &[u8],
) -> Result<(), SessionError> {
    let id = doc_id.as_bytes();
    check_len("doc_id", id.len(), MAX_DOC_ID_LEN)?;
    check_len("payload", payload.len(), MAX_PAYLOAD_LEN)?;
    send.write_all(&(id.len() as u32).to_be_bytes()).await?;
    send.write_all(id).await?;
    send.write_all(&(payload.len() as u32).to_be_bytes())
        .await?;
    send.write_all(payload).await?;
    Ok(())
}

async fn read_frame(recv: &mut RecvStream) -> Result<(String, Vec<u8>), SessionError> {
    let id_len = read_len(recv, "doc_id", MAX_DOC_ID_LEN).await?;
    let mut id = vec![0u8; id_len as usize];
    recv.read_exact(&mut id).await?;
    let doc_id = String::from_utf8(id).map_err(|_| SessionError::BadDocId)?;

    let payload_len = read_len(recv, "payload", MAX_PAYLOAD_LEN).await?;
    let mut payload = vec![0u8; payload_len as usize];
    recv.read_exact(&mut payload).await?;
    Ok((doc_id, payload))
}

async fn read_len(
    recv: &mut RecvStream,
    field: &'static str,
    max: u32,
) -> Result<u32, SessionError> {
    let mut buf = [0u8; 4];
    recv.read_exact(&mut buf).await?;
    let len = u32::from_be_bytes(buf);
    check_len(field, len as usize, max)?;
    Ok(len)
}

fn check_len(field: &'static str, len: usize, max: u32) -> Result<(), SessionError> {
    if len as u64 > max as u64 {
        return Err(SessionError::Oversized {
            field,
            len: len as u32,
            max,
        });
    }
    Ok(())
}
