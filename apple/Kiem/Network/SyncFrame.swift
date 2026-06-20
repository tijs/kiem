import Foundation

/// One framed message on the Kiem sync wire. Byte-for-byte the Rust CLI protocol
/// (`crates/kiem-core/src/protocol.rs`):
///
///     [docIdLen: u32 BE][docId UTF-8][payloadLen: u32 BE][payload]
///
/// A **control** frame has an empty `docId`; its payload is the sender's peer id
/// (the handshake "hello"). A **data** frame carries a document UUID and an
/// Automerge sync message as its payload.
struct SyncFrame {
    let docId: String
    let payload: Data

    var isControl: Bool { docId.isEmpty }

    /// The handshake hello: an empty docId with the peer id as the payload.
    static func control(peerId: String) -> SyncFrame {
        SyncFrame(docId: "", payload: Data(peerId.utf8))
    }

    /// Encode to wire bytes (two big-endian u32 length prefixes).
    func encoded() -> Data {
        let id = Data(docId.utf8)
        var out = Data(capacity: 8 + id.count + payload.count)
        out.append(UInt32(id.count).bigEndianData)
        out.append(id)
        out.append(UInt32(payload.count).bigEndianData)
        out.append(payload)
        return out
    }
}

enum SyncProtocolError: Error {
    /// A length prefix exceeded the protocol maximum — corrupt or hostile peer.
    case frameTooLarge
    /// The docId bytes were not valid UTF-8.
    case badDocId
}

/// Accumulates bytes from the network and yields complete frames. The transport
/// is a raw byte stream, so one `receive` may deliver a partial frame or several
/// frames back to back; this buffers until each frame is whole.
struct SyncFrameDecoder {
    private var buffer = Data()

    // Mirror the Rust guards (protocol.rs): reject oversized prefixes before
    // allocating, so a bad peer can't ask us to buffer gigabytes.
    private static let maxDocIdLen: UInt32 = 1024
    private static let maxPayloadLen: UInt32 = 64 * 1024 * 1024

    mutating func append(_ data: Data) {
        buffer.append(data)
    }

    /// Pull the next complete frame, or `nil` if more bytes are still needed.
    /// Throws if a length prefix is out of range.
    mutating func next() throws -> SyncFrame? {
        guard let idLen = peekUInt32(at: 0) else { return nil }
        guard idLen <= Self.maxDocIdLen else { throw SyncProtocolError.frameTooLarge }

        let idEnd = 4 + Int(idLen)
        guard let payloadLen = peekUInt32(at: idEnd) else { return nil }
        guard payloadLen <= Self.maxPayloadLen else { throw SyncProtocolError.frameTooLarge }

        let payloadStart = idEnd + 4
        let frameEnd = payloadStart + Int(payloadLen)
        guard buffer.count >= frameEnd else { return nil }

        let idData = buffer.subdata(in: 4 ..< idEnd)
        let payload = buffer.subdata(in: payloadStart ..< frameEnd)
        buffer.removeSubrange(0 ..< frameEnd)

        guard let docId = String(data: idData, encoding: .utf8) else {
            throw SyncProtocolError.badDocId
        }
        return SyncFrame(docId: docId, payload: payload)
    }

    /// Read a big-endian u32 at `offset` from the buffer's logical start, or nil
    /// if fewer than 4 bytes are available there.
    private func peekUInt32(at offset: Int) -> UInt32? {
        guard buffer.count >= offset + 4 else { return nil }
        let base = buffer.startIndex + offset
        return (UInt32(buffer[base]) << 24)
            | (UInt32(buffer[base + 1]) << 16)
            | (UInt32(buffer[base + 2]) << 8)
            | UInt32(buffer[base + 3])
    }
}

private extension UInt32 {
    /// The four big-endian bytes of this value.
    var bigEndianData: Data {
        Data([
            UInt8((self >> 24) & 0xff),
            UInt8((self >> 16) & 0xff),
            UInt8((self >> 8) & 0xff),
            UInt8(self & 0xff)
        ])
    }
}
