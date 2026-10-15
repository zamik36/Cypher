//! Blind inboxes by epoch.
//!
//! Each device of an identity reads a different inbox every week. The
//! inbox's address is the public half of an Ed25519 key derived from the
//! identity's seed, the device and the epoch: anyone told the address may
//! leave items there, and only the device proves it may read or empty it,
//! by signing the request. The server never holds anything that would let
//! it read the inbox itself later, and addresses do not link one week to
//! the next.

use cypher_types::{DeviceId, PeerId};
use ed25519_dalek::{Signer, SigningKey};

use crate::error::CryptoError;
use crate::identity::{IdentitySeed, verify_signature};
use crate::kdf::hkdf;

/// How long one inbox is the one a device announces.
pub const INBOX_EPOCH_SECS: u64 = 7 * 24 * 3600;
const INBOX_KEY_LABEL: &[u8] = b"cypher/v4/inbox/";
const ACCESS_TOKEN_LABEL: &[u8] = b"cypher/v4/access/";
const INBOX_OP_CONTEXT: &[u8] = b"cypher/v4/inbox-op";

/// The inbox epoch at `now_secs`.
pub fn inbox_epoch(now_secs: u64) -> u32 {
    u32::try_from(now_secs / INBOX_EPOCH_SECS).unwrap_or(u32::MAX)
}

/// What a signed inbox request does; signed so one cannot stand for another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InboxOp {
    Fetch = 1,
    Ack = 2,
    PushRegister = 3,
    PushUnregister = 4,
    /// The last fetch of an inbox no longer announced: it stops taking items.
    Close = 5,
}

/// One device's inbox key for one epoch.
pub struct InboxKey(SigningKey);

impl InboxKey {
    /// The inbox's address: the public key.
    pub fn id(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    /// Signs request `op` on this inbox at `timestamp_secs`; `extra` is
    /// what the request carries besides (a claim, a push subscription).
    pub fn sign_op(&self, op: InboxOp, timestamp_secs: u64, extra: &[u8]) -> [u8; 64] {
        let message = op_message(op, &self.id(), timestamp_secs, extra);
        self.0.sign(&message).to_bytes()
    }
}

impl IdentitySeed {
    /// The key of `device`'s inbox in `epoch`.
    pub fn derive_inbox_key(&self, device: DeviceId, epoch: u32) -> InboxKey {
        InboxKey(SigningKey::from_bytes(&hkdf::<32>(
            None,
            self.as_bytes(),
            &epoch_info(INBOX_KEY_LABEL, device, epoch),
        )))
    }

    /// What lets `device`'s contacts deliver to it sealed in `epoch`: the
    /// device tells its gateway the token's hash, and contacts the token.
    pub fn derive_access_token(&self, device: DeviceId, epoch: u32) -> [u8; 32] {
        *hkdf::<32>(
            None,
            self.as_bytes(),
            &epoch_info(ACCESS_TOKEN_LABEL, device, epoch),
        )
    }
}

fn epoch_info(label: &[u8], device: DeviceId, epoch: u32) -> Vec<u8> {
    [label, &device.0.to_le_bytes(), &epoch.to_le_bytes()].concat()
}

fn op_message(op: InboxOp, inbox: &[u8; 32], timestamp_secs: u64, extra: &[u8]) -> Vec<u8> {
    [
        INBOX_OP_CONTEXT,
        &[op as u8],
        inbox,
        &timestamp_secs.to_le_bytes(),
        extra,
    ]
    .concat()
}

/// What a push registration signs besides: the subscription itself, so a
/// signature cannot be replayed to point the inbox elsewhere.
pub fn push_extra(endpoint: &str, p256dh: &[u8; 65], auth: &[u8; 16]) -> Vec<u8> {
    [endpoint.as_bytes(), p256dh, auth].concat()
}

/// Checks that the owner of `inbox` signed request `op` at `timestamp_secs`.
pub fn verify_op(
    inbox: &[u8; 32],
    op: InboxOp,
    timestamp_secs: u64,
    extra: &[u8],
    signature: &[u8; 64],
) -> Result<(), CryptoError> {
    let message = op_message(op, inbox, timestamp_secs, extra);
    verify_signature(&PeerId(*inbox), &message, signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inboxes_differ_by_device_and_epoch_and_are_stable() {
        let seed = IdentitySeed([7; 32]);
        let a = seed.derive_inbox_key(DeviceId(1), 10).id();
        assert_eq!(a, seed.derive_inbox_key(DeviceId(1), 10).id());
        assert_ne!(a, seed.derive_inbox_key(DeviceId(1), 11).id());
        assert_ne!(a, seed.derive_inbox_key(DeviceId(2), 10).id());
        assert_ne!(
            a,
            IdentitySeed([8; 32]).derive_inbox_key(DeviceId(1), 10).id()
        );
        assert_eq!(inbox_epoch(INBOX_EPOCH_SECS * 3 + 5), 3);
    }

    #[test]
    fn access_tokens_differ_by_device_and_epoch_and_from_the_inbox() {
        let seed = IdentitySeed([7; 32]);
        let token = seed.derive_access_token(DeviceId(1), 10);
        assert_eq!(token, seed.derive_access_token(DeviceId(1), 10));
        assert_ne!(token, seed.derive_access_token(DeviceId(1), 11));
        assert_ne!(token, seed.derive_access_token(DeviceId(2), 10));
        assert_ne!(token, seed.derive_inbox_key(DeviceId(1), 10).id());
    }

    #[test]
    fn only_the_inbox_key_signs_its_requests() {
        let key = IdentitySeed([7; 32]).derive_inbox_key(DeviceId(1), 10);
        let inbox = key.id();
        let sig = key.sign_op(InboxOp::Ack, 100, b"claim");
        verify_op(&inbox, InboxOp::Ack, 100, b"claim", &sig).unwrap();
        verify_op(&inbox, InboxOp::Fetch, 100, b"claim", &sig).unwrap_err();
        verify_op(&inbox, InboxOp::Ack, 101, b"claim", &sig).unwrap_err();
        verify_op(&inbox, InboxOp::Ack, 100, b"other", &sig).unwrap_err();
        let other = IdentitySeed([7; 32]).derive_inbox_key(DeviceId(1), 11).id();
        verify_op(&other, InboxOp::Ack, 100, b"claim", &sig).unwrap_err();
    }
}
