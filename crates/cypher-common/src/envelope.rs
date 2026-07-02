//! Binary envelope for gateway → signaling messages over NATS.
//!
//! Carries the source `session_id` alongside an opaque proto payload. Using a
//! fixed 8-byte big-endian prefix + raw bytes avoids the ~4-6x bloat and the
//! per-message allocation of JSON-encoding a `Vec<u8>` as an array of numbers,
//! which dominated routing cost on the hot path.

use crate::{Error, Result};

/// Size of the fixed `session_id` header prefixing every envelope.
pub const ENVELOPE_HEADER_LEN: usize = 8;

/// A decoded gateway → signaling envelope.
#[derive(Debug, Clone)]
pub struct GatewayEnvelope {
    pub session_id: u64,
    pub payload: Vec<u8>,
}

impl GatewayEnvelope {
    /// Encode `session_id` and `payload` into a single wire buffer:
    /// `session_id` (8 bytes, big-endian) followed by the raw payload.
    pub fn encode(session_id: u64, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(ENVELOPE_HEADER_LEN + payload.len());
        out.extend_from_slice(&session_id.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// Decode an envelope produced by [`encode`](Self::encode).
    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < ENVELOPE_HEADER_LEN {
            return Err(Error::Protocol(format!(
                "gateway envelope too short: {} < {ENVELOPE_HEADER_LEN}",
                data.len()
            )));
        }
        let mut id_bytes = [0u8; ENVELOPE_HEADER_LEN];
        id_bytes.copy_from_slice(&data[..ENVELOPE_HEADER_LEN]);
        Ok(Self {
            session_id: u64::from_be_bytes(id_bytes),
            payload: data[ENVELOPE_HEADER_LEN..].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let payload = vec![0u8, 1, 2, 255, 42];
        let bytes = GatewayEnvelope::encode(0xDEAD_BEEF_1234, &payload);
        assert_eq!(bytes.len(), ENVELOPE_HEADER_LEN + payload.len());
        let decoded = GatewayEnvelope::decode(&bytes).unwrap();
        assert_eq!(decoded.session_id, 0xDEAD_BEEF_1234);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn empty_payload_roundtrips() {
        let bytes = GatewayEnvelope::encode(7, &[]);
        let decoded = GatewayEnvelope::decode(&bytes).unwrap();
        assert_eq!(decoded.session_id, 7);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn short_buffer_is_rejected() {
        assert!(GatewayEnvelope::decode(&[0u8; 4]).is_err());
    }
}
