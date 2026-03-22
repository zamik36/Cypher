//! Binary protocol serialization for the p2p system.
//!
//! Wire format (little-endian):
//! - constructor_id: u32
//! - Int -> 4 bytes LE
//! - Long -> 8 bytes LE
//! - Bytes/String -> u32 length prefix + data + padding to 4-byte alignment

/// Trait for types that can be serialized/deserialized in the wire format.
pub trait Serializable: Sized {
    /// Serialize this value into a byte vector (including constructor ID).
    fn serialize(&self) -> Vec<u8>;

    /// Deserialize from raw bytes (including constructor ID).
    fn deserialize(data: &[u8]) -> cypher_common::Result<Self>;
}

/// Encode a byte slice with a u32 length prefix and padding to 4-byte alignment.
pub fn encode_bytes(buf: &mut Vec<u8>, data: &[u8]) {
    let len = data.len() as u32;
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(data);
    // Pad to 4-byte alignment
    let padding = (4 - (data.len() % 4)) % 4;
    buf.extend(std::iter::repeat_n(0u8, padding));
}

/// Decode length-prefixed bytes from a buffer at the given offset.
/// Returns the decoded bytes and the new offset (after data + padding).
pub fn decode_bytes(buf: &[u8], offset: usize) -> cypher_common::Result<(Vec<u8>, usize)> {
    if offset + 4 > buf.len() {
        return Err(cypher_common::Error::Protocol(
            "truncated bytes length".into(),
        ));
    }
    let len = u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap()) as usize;
    let data_start = offset + 4;
    let data_end = data_start + len;
    if data_end > buf.len() {
        return Err(cypher_common::Error::Protocol(
            "truncated bytes data".into(),
        ));
    }
    let data = buf[data_start..data_end].to_vec();
    let padding = (4 - (len % 4)) % 4;
    let new_offset = data_end + padding;
    Ok((data, new_offset))
}

/// Encode a string with a u32 length prefix and padding to 4-byte alignment.
pub fn encode_string(buf: &mut Vec<u8>, s: &str) {
    encode_bytes(buf, s.as_bytes());
}

/// Decode a length-prefixed UTF-8 string from a buffer at the given offset.
/// Returns the decoded string and the new offset (after data + padding).
pub fn decode_string(buf: &[u8], offset: usize) -> cypher_common::Result<(String, usize)> {
    let (bytes, new_offset) = decode_bytes(buf, offset)?;
    let s = String::from_utf8(bytes)
        .map_err(|e| cypher_common::Error::Protocol(format!("invalid UTF-8 string: {}", e)))?;
    Ok((s, new_offset))
}

include!(concat!(env!("OUT_DIR"), "/proto_generated.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_bytes() {
        let mut buf = Vec::new();
        let data = b"hello";
        encode_bytes(&mut buf, data);
        // length (5) + data (5) + padding (3) = 12 bytes
        assert_eq!(buf.len(), 12);
        let (decoded, offset) = decode_bytes(&buf, 0).unwrap();
        assert_eq!(decoded, data);
        assert_eq!(offset, 12);
    }

    #[test]
    fn test_encode_decode_bytes_aligned() {
        let mut buf = Vec::new();
        let data = b"test"; // 4 bytes, already aligned
        encode_bytes(&mut buf, data);
        assert_eq!(buf.len(), 8); // 4 length + 4 data, no padding
        let (decoded, offset) = decode_bytes(&buf, 0).unwrap();
        assert_eq!(decoded, data);
        assert_eq!(offset, 8);
    }

    #[test]
    fn test_encode_decode_string() {
        let mut buf = Vec::new();
        encode_string(&mut buf, "hello world");
        let (decoded, _) = decode_string(&buf, 0).unwrap();
        assert_eq!(decoded, "hello world");
    }

    #[test]
    fn test_session_init_roundtrip() {
        let msg = SessionInit {
            client_id: vec![1, 2, 3, 4],
            nonce: vec![5, 6, 7, 8, 9, 10, 11, 12],
        };
        let data = msg.serialize();
        let decoded = SessionInit::deserialize(&data).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_chat_send_roundtrip() {
        let msg = ChatSend {
            peer_id: vec![0xAA; 32],
            ciphertext: vec![0xBB; 100],
            ratchet_key: vec![0xCC; 32],
            msg_no: 42,
        };
        let data = msg.serialize();
        let decoded = ChatSend::deserialize(&data).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_dispatch() {
        let msg = SignalRequestPeer {
            link_id: "test-link-123".to_string(),
        };
        let data = msg.serialize();
        let dispatched = dispatch(&data).unwrap();
        match dispatched {
            Message::SignalRequestPeer(inner) => {
                assert_eq!(inner.link_id, "test-link-123");
            }
            other => panic!("unexpected variant: {:?}", other),
        }
    }

    #[test]
    fn test_dispatch_unknown_id() {
        let data = [0xFF, 0xFF, 0xFF, 0xFF];
        assert!(dispatch(&data).is_err());
    }

    #[test]
    fn test_chat_receive_empty_fields() {
        let msg = ChatReceive {};
        let data = msg.serialize();
        assert_eq!(data.len(), 4); // just constructor ID
        let decoded = ChatReceive::deserialize(&data).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_file_offer_roundtrip() {
        let msg = FileOffer {
            peer_id: vec![0xAA, 0xBB],
            file_id: vec![1, 2, 3],
            name: "document.pdf".to_string(),
            size: 1_000_000,
            chunks: 250,
            hash: vec![0xDE, 0xAD, 0xBE, 0xEF],
            compressed: 1,
        };
        let data = msg.serialize();
        let decoded = FileOffer::deserialize(&data).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_message_serialize() {
        let msg = Message::SessionInit(SessionInit {
            client_id: vec![1, 2, 3],
            nonce: vec![4, 5, 6],
        });
        let data = msg.serialize();
        let dispatched = dispatch(&data).unwrap();
        assert_eq!(msg, dispatched);
    }
}
