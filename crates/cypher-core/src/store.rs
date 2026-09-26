use cypher_crypto::aead;
use cypher_types::{MsgId, PeerId};
use rand_core::CryptoRngCore;
use serde::Serialize;
use serde::de::DeserializeOwned;
use zeroize::Zeroizing;

use crate::CoreError;
use crate::api::StoredMessage;

/// Logical tables of the driver's ordered key-value store. Keys sort
/// lexicographically as raw bytes; values are always [`Vault`]-sealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Table {
    Meta = 1,
    Peers = 2,
    Outbox = 3,
    Transfers = 4,
    /// Key: `peer ‖ sent_at_ms (BE) ‖ msg_id`, so a prefix scan by peer
    /// yields a conversation in chronological order.
    Messages = 5,
    /// Key: `msg_id` → latest [`crate::MessageStatus`]; overlays the
    /// status stored with the message itself.
    MessageStatus = 6,
}

impl Table {
    pub const ALL: [Self; 6] = [
        Self::Meta,
        Self::Peers,
        Self::Outbox,
        Self::Transfers,
        Self::Messages,
        Self::MessageStatus,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Meta => "meta",
            Self::Peers => "peers",
            Self::Outbox => "outbox",
            Self::Transfers => "transfers",
            Self::Messages => "messages",
            Self::MessageStatus => "message_status",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreOp {
    Put {
        table: Table,
        key: Vec<u8>,
        value: Vec<u8>,
    },
    Delete {
        table: Table,
        key: Vec<u8>,
    },
}

pub(crate) const META_PREKEYS: &[u8] = b"prekeys";

pub fn message_key(peer: &PeerId, sent_at_ms: u64, msg_id: &MsgId) -> Vec<u8> {
    let mut k = Vec::with_capacity(32 + 8 + 16);
    k.extend_from_slice(peer.as_bytes());
    k.extend_from_slice(&sent_at_ms.to_be_bytes());
    k.extend_from_slice(msg_id.as_bytes());
    k
}

/// At-rest encryption for every record the core persists. Records are bound
/// to their `(table, key)` through the AEAD associated data, so a storage
/// attacker cannot swap values between keys.
pub struct Vault {
    key: Zeroizing<[u8; 32]>,
}

const VAULT_NONCE_LEN: usize = aead::NONCE_LEN;

impl Vault {
    pub fn new(storage_key: [u8; 32]) -> Self {
        Self {
            key: Zeroizing::new(storage_key),
        }
    }

    pub fn seal_bytes(
        &self,
        table: Table,
        key: &[u8],
        plaintext: &[u8],
        rng: &mut impl CryptoRngCore,
    ) -> Vec<u8> {
        let mut nonce = [0u8; VAULT_NONCE_LEN];
        rng.fill_bytes(&mut nonce);
        let mut out = Vec::with_capacity(VAULT_NONCE_LEN + plaintext.len() + aead::TAG_LEN);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(plaintext);
        let tag = aead::seal_detached(
            &self.key,
            &nonce,
            &aad(table, key),
            &mut out[VAULT_NONCE_LEN..],
        );
        out.extend_from_slice(&tag);
        out
    }

    pub fn open_bytes(
        &self,
        table: Table,
        key: &[u8],
        sealed: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, CoreError> {
        if sealed.len() < VAULT_NONCE_LEN + aead::TAG_LEN {
            return Err(CoreError::Storage);
        }
        let (nonce, body) = sealed.split_at(VAULT_NONCE_LEN);
        let nonce: &[u8; VAULT_NONCE_LEN] = nonce.try_into().map_err(|_| CoreError::Storage)?;
        aead::open(&self.key, nonce, &aad(table, key), body)
            .map(Zeroizing::new)
            .map_err(|_| CoreError::Storage)
    }

    pub fn seal<T: Serialize>(
        &self,
        table: Table,
        key: &[u8],
        value: &T,
        rng: &mut impl CryptoRngCore,
    ) -> Vec<u8> {
        let plain = Zeroizing::new(postcard::to_allocvec(value).expect("in-memory serialization"));
        self.seal_bytes(table, key, &plain, rng)
    }

    pub fn open<T: DeserializeOwned>(
        &self,
        table: Table,
        key: &[u8],
        sealed: &[u8],
    ) -> Result<T, CoreError> {
        let plain = self.open_bytes(table, key, sealed)?;
        postcard::from_bytes(&plain).map_err(|_| CoreError::Storage)
    }

    pub fn put<T: Serialize>(
        &self,
        table: Table,
        key: Vec<u8>,
        value: &T,
        rng: &mut impl CryptoRngCore,
    ) -> StoreOp {
        let value = self.seal(table, &key, value, rng);
        StoreOp::Put { table, key, value }
    }

    /// Decrypts a message record read by the driver for history views.
    pub fn open_message(&self, key: &[u8], sealed: &[u8]) -> Result<StoredMessage, CoreError> {
        self.open(Table::Messages, key, sealed)
    }
}

fn aad(table: Table, key: &[u8]) -> Vec<u8> {
    let mut a = Vec::with_capacity(1 + key.len());
    a.push(table as u8);
    a.extend_from_slice(key);
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn vault_roundtrip_is_bound_to_table_and_key() {
        let v = Vault::new([7; 32]);
        let sealed = v.seal(Table::Peers, b"k1", &("hello", 42u32), &mut OsRng);
        let back: (String, u32) = v.open(Table::Peers, b"k1", &sealed).unwrap();
        assert_eq!(back, ("hello".to_string(), 42));
        assert!(
            v.open::<(String, u32)>(Table::Peers, b"k2", &sealed)
                .is_err()
        );
        assert!(
            v.open::<(String, u32)>(Table::Outbox, b"k1", &sealed)
                .is_err()
        );
        assert!(
            Vault::new([8; 32])
                .open::<(String, u32)>(Table::Peers, b"k1", &sealed)
                .is_err()
        );
        assert!(v.open_bytes(Table::Peers, b"k1", &sealed[..5]).is_err());
    }

    #[test]
    fn stored_messages_roundtrip_through_the_vault() {
        use crate::api::{Content, MediaKind, MessageStatus, StoredMessage};
        let v = Vault::new([1; 32]);
        for content in [
            Content::Text {
                text: "hi".into(),
                reply_to: Some(MsgId([2; 16])),
            },
            Content::File {
                file_id: cypher_types::FileId([3; 16]),
                name: "v.webm".into(),
                mime: "audio/webm".into(),
                size: 10,
                kind: MediaKind::Voice {
                    duration_ms: 1,
                    waveform: vec![1, 2],
                },
            },
        ] {
            let msg = StoredMessage {
                msg_id: MsgId([4; 16]),
                peer: PeerId([5; 32]),
                outgoing: true,
                sent_at_ms: 6,
                status: MessageStatus::Read,
                content,
            };
            let key = message_key(&msg.peer, msg.sent_at_ms, &msg.msg_id);
            let sealed = v.seal(Table::Messages, &key, &msg, &mut OsRng);
            assert_eq!(v.open_message(&key, &sealed).unwrap(), msg);
        }
    }

    #[test]
    fn message_keys_sort_chronologically_per_peer() {
        let p = PeerId([1; 32]);
        let a = message_key(&p, 5, &MsgId([9; 16]));
        let b = message_key(&p, 300, &MsgId([0; 16]));
        assert!(a < b);
        assert!(a.starts_with(p.as_bytes()));
    }
}
