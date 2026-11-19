//! Platform-neutral primitives shared by every Cypher crate, including the
//! WebAssembly build: identifiers, the error type and protocol constants.

mod error;
mod id;

pub use error::{Error, Result};
pub use id::{Addr, DeviceId, FileId, LinkId, MsgId, PeerId, base32};

/// Domain-separation prefix for the gateway session proof-of-possession.
pub const SESSION_AUTH_CONTEXT: &[u8] = b"cypher-session-auth-v3";

/// Transcript signed to authenticate `device` against the gateway's `nonce`.
pub fn session_auth_message(nonce: &[u8; 32], device: DeviceId) -> [u8; 58] {
    let mut message = [0u8; 58];
    let (context, rest) = message.split_at_mut(SESSION_AUTH_CONTEXT.len());
    let (challenge, device_id) = rest.split_at_mut(nonce.len());
    context.copy_from_slice(SESSION_AUTH_CONTEXT);
    challenge.copy_from_slice(nonce);
    device_id.copy_from_slice(&device.0.to_le_bytes());
    message
}

/// Most devices one identity can have.
pub const MAX_DEVICES: usize = 6;

/// Upper bound for a single transport frame payload.
pub const MAX_FRAME_SIZE: usize = 1024 * 1024;

pub use hex;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_auth_transcript_has_the_protocol_bytes() {
        let signed = session_auth_message(&[0xa5; 32], DeviceId(0x1234_5678));
        let expected = hex::decode(concat!(
            "6379706865722d73657373696f6e2d617574682d7633",
            "a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5",
            "78563412"
        ))
        .unwrap();
        assert_eq!(signed.as_slice(), expected);
    }
}
