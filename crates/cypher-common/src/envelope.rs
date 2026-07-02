//! Binary envelope for gateway → signaling messages over NATS.
//!
//! Carries the source gateway `node_id` and `session_id` alongside an opaque
//! proto payload. Using a compact binary framing (instead of JSON) avoids the
//! ~4-6x bloat and the per-message allocation of JSON-encoding a `Vec<u8>` as an
//! array of numbers, which dominated routing cost on the hot path.
//!
//! The `node_id` lets signaling reply to the exact gateway that owns the source
//! session — required once more than one gateway replica runs, since session ids
//! are only unique within a node.

use crate::{Error, Result};

/// A decoded gateway → signaling envelope.
#[derive(Debug, Clone)]
pub struct GatewayEnvelope {
    /// Identifier of the gateway node that owns the source session.
    pub node_id: String,
    pub session_id: u64,
    pub payload: Vec<u8>,
}

impl GatewayEnvelope {
    /// Encode into a single wire buffer:
    /// `node_len` (2 bytes BE) | `node_id` | `session_id` (8 bytes BE) | payload.
    pub fn encode(node_id: &str, session_id: u64, payload: &[u8]) -> Vec<u8> {
        let node = node_id.as_bytes();
        let mut out = Vec::with_capacity(2 + node.len() + 8 + payload.len());
        out.extend_from_slice(&(node.len() as u16).to_be_bytes());
        out.extend_from_slice(node);
        out.extend_from_slice(&session_id.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// Decode an envelope produced by [`encode`](Self::encode).
    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < 2 {
            return Err(Error::Protocol(
                "gateway envelope: missing node length".into(),
            ));
        }
        let node_len = u16::from_be_bytes([data[0], data[1]]) as usize;
        let rest = &data[2..];
        if rest.len() < node_len + 8 {
            return Err(Error::Protocol(format!(
                "gateway envelope too short: need node({node_len}) + 8, have {}",
                rest.len()
            )));
        }
        let node_id = String::from_utf8(rest[..node_len].to_vec())
            .map_err(|_| Error::Protocol("gateway envelope: node_id not UTF-8".into()))?;
        let mut id_bytes = [0u8; 8];
        id_bytes.copy_from_slice(&rest[node_len..node_len + 8]);
        Ok(Self {
            node_id,
            session_id: u64::from_be_bytes(id_bytes),
            payload: rest[node_len + 8..].to_vec(),
        })
    }
}

/// NATS subject a gateway subscribes to for a given owned session, and that
/// signaling publishes replies/forwards to. Namespaced by `node_id` so that
/// session ids (unique only per node) never collide across gateway replicas.
pub fn gateway_session_subject(node_id: &str, session_id: u64) -> String {
    format!("gateway.{node_id}.session.{session_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let payload = vec![0u8, 1, 2, 255, 42];
        let bytes = GatewayEnvelope::encode("gateway-3", 0xDEAD_BEEF_1234, &payload);
        let decoded = GatewayEnvelope::decode(&bytes).unwrap();
        assert_eq!(decoded.node_id, "gateway-3");
        assert_eq!(decoded.session_id, 0xDEAD_BEEF_1234);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn empty_payload_and_node_roundtrips() {
        let bytes = GatewayEnvelope::encode("", 7, &[]);
        let decoded = GatewayEnvelope::decode(&bytes).unwrap();
        assert_eq!(decoded.node_id, "");
        assert_eq!(decoded.session_id, 7);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn short_buffer_is_rejected() {
        assert!(GatewayEnvelope::decode(&[0u8; 1]).is_err());
        // Claims a 5-byte node but supplies nothing after the length prefix.
        assert!(GatewayEnvelope::decode(&[0u8, 5]).is_err());
    }

    #[test]
    fn subject_is_node_namespaced() {
        assert_eq!(
            gateway_session_subject("gateway-0", 42),
            "gateway.gateway-0.session.42"
        );
    }
}
