//! Cypher wire protocol v3: the only messages the server infrastructure
//! understands. Everything end-to-end lives inside opaque `body` fields.
//!
//! Layout: `[kind u8][req_id u32 LE][fields...]`, little-endian integers,
//! `u32`-prefixed byte strings. Decoding is zero-copy for payload fields.

// Parses untrusted input: arithmetic must say how it handles overflow, and
// every match must name each message kind so new ones cannot slip through.
#![cfg_attr(
    not(test),
    deny(clippy::arithmetic_side_effects, clippy::wildcard_enum_match_arm)
)]

mod codec;
mod message;

pub use message::{
    ClientMsg, DeliveryStatus, ErrorCode, Frame, SEND_HEADER_LEN, SendView, ServerMsg, encode_recv,
    peek_send, relay_addr_is_valid,
};

/// Bumped on any incompatible change; the gateway rejects other versions.
pub const PROTOCOL_VERSION: u16 = 3;

/// Size of the fixed frame prefix (`kind` + `req_id`).
pub const FRAME_HEADER_LEN: usize = 5;

/// Encoded prekey bundle without the one-time prekey part: identity(32) ‖
/// device(4) ‖ `identity_dh(32)` ‖ `spk_id(4)` ‖ spk(32) ‖ signature(64).
pub const BUNDLE_BASE_LEN: usize = 168;

pub const MAX_BODY_LEN: usize = cypher_types::MAX_FRAME_SIZE - FRAME_HEADER_LEN - 64;
pub const MAX_INBOX_ITEM_LEN: usize = 72 * 1024;
pub const MAX_INBOX_BATCH: usize = 64;
pub const MAX_OPKS_PER_PUBLISH: usize = 200;
pub const MAX_RELAY_ADDR_LEN: usize = 255;
/// Longest signed device list: identity, version, count, ids, signature.
pub const MAX_DEVICE_LIST_LEN: usize = 32 + 8 + 1 + 4 * cypher_types::MAX_DEVICES + 64;
/// Longest Web Push endpoint URL a client may register.
pub const MAX_PUSH_ENDPOINT_LEN: usize = 1024;

/// Public inbox address derived from the owner's secret. Writers need only
/// the id; fetching or acknowledging requires the preimage.
pub fn inbox_id(secret: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::new()
        .chain_update(b"cypher/v2/inbox")
        .chain_update(secret)
        .finalize()
        .into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("truncated frame")]
    Truncated,
    #[error("unknown message kind {0:#04x}")]
    UnknownKind(u8),
    #[error("field too large")]
    TooLarge,
    #[error("trailing bytes")]
    TrailingBytes,
    #[error("malformed field")]
    Malformed,
}

impl From<WireError> for cypher_types::Error {
    fn from(e: WireError) -> Self {
        Self::Protocol(e.to_string())
    }
}
