//! Framed message format for multiplexing per-document sync over one stream.
//!
//! Wire format (all lengths big-endian u32):
//! `[doc_id_len][doc_id bytes][payload_len][payload bytes]`
//!
//! A frame with an **empty doc_id** is a control frame; the payload carries
//! the control content (today: the peer-id hello sent once per connection).
//! Length limits guard against garbage on the port — a bad frame is an error,
//! not an allocation.

use std::io::{Read, Write};

/// Note ids are UUIDs today; leave generous headroom. `pub` so other
/// transports (e.g. kiem-sync's async iroh codec) can enforce the same limits
/// without duplicating the numbers.
pub const MAX_DOC_ID_LEN: u32 = 1024;
/// A full document snapshot travels inside one sync message.
pub const MAX_PAYLOAD_LEN: u32 = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame field too large: {field} is {len} bytes (max {max})")]
    Oversized { field: &'static str, len: u32, max: u32 },
    #[error("doc id is not valid UTF-8")]
    BadDocId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Empty string marks a control frame.
    pub doc_id: String,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn control(payload: impl Into<Vec<u8>>) -> Frame {
        Frame { doc_id: String::new(), payload: payload.into() }
    }

    pub fn is_control(&self) -> bool {
        self.doc_id.is_empty()
    }
}

pub fn write_frame(w: &mut impl Write, frame: &Frame) -> Result<(), ProtocolError> {
    let id = frame.doc_id.as_bytes();
    check_len("doc_id", id.len(), MAX_DOC_ID_LEN)?;
    check_len("payload", frame.payload.len(), MAX_PAYLOAD_LEN)?;
    w.write_all(&(id.len() as u32).to_be_bytes())?;
    w.write_all(id)?;
    w.write_all(&(frame.payload.len() as u32).to_be_bytes())?;
    w.write_all(&frame.payload)?;
    w.flush()?;
    Ok(())
}

pub fn read_frame(r: &mut impl Read) -> Result<Frame, ProtocolError> {
    let id_len = read_len(r, "doc_id", MAX_DOC_ID_LEN)?;
    let mut id = vec![0u8; id_len as usize];
    r.read_exact(&mut id)?;
    let doc_id = String::from_utf8(id).map_err(|_| ProtocolError::BadDocId)?;

    let payload_len = read_len(r, "payload", MAX_PAYLOAD_LEN)?;
    let mut payload = vec![0u8; payload_len as usize];
    r.read_exact(&mut payload)?;
    Ok(Frame { doc_id, payload })
}

fn read_len(r: &mut impl Read, field: &'static str, max: u32) -> Result<u32, ProtocolError> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    let len = u32::from_be_bytes(buf);
    check_len(field, len as usize, max)?;
    Ok(len)
}

fn check_len(field: &'static str, len: usize, max: u32) -> Result<(), ProtocolError> {
    if len as u64 > max as u64 {
        return Err(ProtocolError::Oversized { field, len: len as u32, max });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(frame: &Frame) -> Frame {
        let mut buf = Vec::new();
        write_frame(&mut buf, frame).unwrap();
        read_frame(&mut buf.as_slice()).unwrap()
    }

    #[test]
    fn data_frame_roundtrips() {
        let frame = Frame { doc_id: "note-123".into(), payload: vec![1, 2, 3, 255, 0] };
        assert_eq!(roundtrip(&frame), frame);
    }

    #[test]
    fn control_frame_roundtrips_and_is_flagged() {
        let frame = Frame::control(b"peer-abc".to_vec());
        let back = roundtrip(&frame);
        assert!(back.is_control());
        assert_eq!(back.payload, b"peer-abc");
    }

    #[test]
    fn empty_payload_roundtrips() {
        let frame = Frame { doc_id: "d".into(), payload: vec![] };
        assert_eq!(roundtrip(&frame), frame);
    }

    #[test]
    fn multiple_frames_stream_in_order() {
        let frames = vec![
            Frame::control(b"hello".to_vec()),
            Frame { doc_id: "a".into(), payload: vec![1] },
            Frame { doc_id: "b".into(), payload: vec![2, 3] },
        ];
        let mut buf = Vec::new();
        for f in &frames {
            write_frame(&mut buf, f).unwrap();
        }
        let mut cursor = buf.as_slice();
        for f in &frames {
            assert_eq!(&read_frame(&mut cursor).unwrap(), f);
        }
    }

    #[test]
    fn oversized_length_prefix_is_rejected_not_allocated() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            read_frame(&mut buf.as_slice()),
            Err(ProtocolError::Oversized { field: "doc_id", .. })
        ));
    }

    #[test]
    fn truncated_stream_is_an_io_error() {
        let frame = Frame { doc_id: "doc".into(), payload: vec![1, 2, 3, 4] };
        let mut buf = Vec::new();
        write_frame(&mut buf, &frame).unwrap();
        buf.truncate(buf.len() - 2);
        assert!(matches!(read_frame(&mut buf.as_slice()), Err(ProtocolError::Io(_))));
    }
}
