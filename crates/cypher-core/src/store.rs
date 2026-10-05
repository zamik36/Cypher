use std::cmp::Ordering;

use cypher_crypto::aead;
use cypher_types::{FileId, MsgId, PeerId};
use rand_core::CryptoRngCore;
use serde::Serialize;
use serde::de::DeserializeOwned;
use zeroize::Zeroizing;

use crate::CoreError;
use crate::api::StoredMessage;
use crate::media::MediaKey;

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
    /// Key: `file_id` → [`crate::MediaKey`] of a voice or video note kept
    /// sealed at rest.
    Media = 7,
}

impl Table {
    pub const ALL: [Self; 7] = [
        Self::Meta,
        Self::Peers,
        Self::Outbox,
        Self::Transfers,
        Self::Messages,
        Self::MessageStatus,
        Self::Media,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Meta => "meta",
            Self::Peers => "peers",
            Self::Outbox => "outbox",
            Self::Transfers => "transfers",
            Self::Messages => "messages",
            Self::MessageStatus => "message_status",
            Self::Media => "media",
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
pub(crate) const META_PROFILE: &[u8] = b"profile";

/// How a contact is called: the name the user gave them, the one they go by.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContactNames {
    pub alias: Option<String>,
    pub name: Option<String>,
}

/// A type persisted through the [`Vault`]. Its plaintext is
/// `VERSION ‖ postcard(self)`: bump [`Record::VERSION`] whenever the
/// serialized shape changes, and teach [`Record::upgrade`] to read the
/// previous shapes so data written by older releases keeps loading.
pub trait Record: Serialize + DeserializeOwned {
    const VERSION: u8;

    /// Decodes a record written as `version`, always older than
    /// [`Record::VERSION`]. Without a migration the record is unreadable.
    fn upgrade(_version: u8, _body: &[u8]) -> Result<Self, CoreError> {
        Err(CoreError::Storage)
    }
}

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
        let (_, body) = out.split_at_mut(VAULT_NONCE_LEN);
        let tag = aead::seal_detached(&self.key, &nonce, &aad(table, key), body);
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

    #[expect(
        clippy::expect_used,
        reason = "postcard serialization into a Vec cannot fail for our record types"
    )]
    pub fn seal<T: Record>(
        &self,
        table: Table,
        key: &[u8],
        value: &T,
        rng: &mut impl CryptoRngCore,
    ) -> Vec<u8> {
        let plain = Zeroizing::new(
            postcard::to_extend(value, vec![T::VERSION]).expect("in-memory serialization"),
        );
        self.seal_bytes(table, key, &plain, rng)
    }

    /// Decrypts and decodes a record, upgrading older versions. A record
    /// written by a newer release is [`CoreError::NewerStorage`].
    pub fn open<T: Record>(&self, table: Table, key: &[u8], sealed: &[u8]) -> Result<T, CoreError> {
        let plain = self.open_bytes(table, key, sealed)?;
        let (&version, body) = plain.split_first().ok_or(CoreError::Storage)?;
        match version.cmp(&T::VERSION) {
            Ordering::Equal => postcard::from_bytes(body).map_err(|_| CoreError::Storage),
            Ordering::Less => T::upgrade(version, body),
            Ordering::Greater => Err(CoreError::NewerStorage),
        }
    }

    pub fn put<T: Record>(
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

    /// Decrypts the playback key of a sealed media file.
    pub fn open_media(&self, file_id: &FileId, sealed: &[u8]) -> Result<MediaKey, CoreError> {
        self.open(Table::Media, file_id.as_bytes(), sealed)
    }

    /// A contact's names from its sealed session row (keyed by the peer id).
    /// Drivers list contacts without restoring sessions.
    pub fn open_contact(&self, peer_key: &[u8], sealed: &[u8]) -> Result<ContactNames, CoreError> {
        self.open::<crate::peer::PeerRecord>(Table::Peers, peer_key, sealed)
            .map(|record| ContactNames {
                alias: record.alias,
                name: record.name,
            })
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
    use serde::Deserialize;

    /// Version 2 of a test record; version 1 stored the count as a `u8`.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Note {
        text: String,
        count: u32,
    }

    impl Record for Note {
        const VERSION: u8 = 2;

        fn upgrade(version: u8, body: &[u8]) -> Result<Self, CoreError> {
            match version {
                1 => {
                    let (text, count): (String, u8) =
                        postcard::from_bytes(body).map_err(|_| CoreError::Storage)?;
                    Ok(Self {
                        text,
                        count: count.into(),
                    })
                }
                _ => Err(CoreError::Storage),
            }
        }
    }

    fn note() -> Note {
        Note {
            text: "hello".into(),
            count: 42,
        }
    }

    /// Seals raw `version ‖ body` the way an older or newer release would.
    fn sealed_as(v: &Vault, version: u8, body: &[u8]) -> Vec<u8> {
        let plain = [&[version][..], body].concat();
        v.seal_bytes(Table::Peers, b"k1", &plain, &mut OsRng)
    }

    #[test]
    fn vault_roundtrip_is_bound_to_table_and_key() {
        let v = Vault::new([7; 32]);
        let sealed = v.seal(Table::Peers, b"k1", &note(), &mut OsRng);
        assert_eq!(
            v.open::<Note>(Table::Peers, b"k1", &sealed).unwrap(),
            note()
        );
        v.open::<Note>(Table::Peers, b"k2", &sealed).unwrap_err();
        v.open::<Note>(Table::Outbox, b"k1", &sealed).unwrap_err();
        Vault::new([8; 32])
            .open::<Note>(Table::Peers, b"k1", &sealed)
            .unwrap_err();
        v.open_bytes(Table::Peers, b"k1", &sealed[..5]).unwrap_err();
    }

    #[test]
    fn records_carry_their_version_and_older_ones_are_upgraded() {
        let v = Vault::new([7; 32]);
        let old = postcard::to_allocvec(&("hello", 42u8)).unwrap();
        let upgraded: Note = v
            .open(Table::Peers, b"k1", &sealed_as(&v, 1, &old))
            .unwrap();
        assert_eq!(upgraded, note());

        let unknown_old = sealed_as(&v, 0, &old);
        assert!(matches!(
            v.open::<Note>(Table::Peers, b"k1", &unknown_old),
            Err(CoreError::Storage)
        ));
        let newer = sealed_as(&v, 3, &postcard::to_allocvec(&note()).unwrap());
        assert!(matches!(
            v.open::<Note>(Table::Peers, b"k1", &newer),
            Err(CoreError::NewerStorage)
        ));
        assert!(matches!(
            v.open::<Note>(Table::Peers, b"k1", &sealed_as(&v, 2, b"")),
            Err(CoreError::Storage)
        ));
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
                file_id: FileId([3; 16]),
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
