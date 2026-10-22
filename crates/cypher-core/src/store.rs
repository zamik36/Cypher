use std::cmp::Ordering;

use cypher_crypto::aead;
use cypher_types::{Addr, DeviceId, FileId, MsgId, PeerId};
use rand_core::CryptoRngCore;
use serde::Serialize;
use serde::de::DeserializeOwned;
use zeroize::Zeroizing;

use crate::CoreError;
use crate::api::{Content, MessageStatus, StoredMessage};
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
    /// Key: `peer ‖ device (BE)` → the session with that device of a
    /// contact (or of our own identity), see [`session_key`]; the keys it
    /// keeps for messages that have not come follow it, one row each
    /// (`session_key ‖ 1 ‖ seq (BE)`).
    Sessions = 8,
}

impl Table {
    pub const ALL: [Self; 8] = [
        Self::Meta,
        Self::Peers,
        Self::Outbox,
        Self::Transfers,
        Self::Messages,
        Self::MessageStatus,
        Self::Media,
        Self::Sessions,
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
            Self::Sessions => "sessions",
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
pub(crate) const META_LINKS: &[u8] = b"links";
pub(crate) const META_DEVICES: &[u8] = b"devices";
pub(crate) const META_HISTORY: &[u8] = b"history";
pub(crate) const META_SERVERS: &[u8] = b"servers";
pub(crate) const META_INBOX: &[u8] = b"inbox";

/// What the UI shows of a contact: the name the user gave them, the one
/// they go by, and whether they are a request or blocked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContactInfo {
    pub alias: Option<String>,
    pub name: Option<String>,
    pub request: bool,
    pub blocked: bool,
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

/// Where the session with one device is stored; a prefix scan by peer
/// yields all of that peer's sessions.
pub fn session_key(addr: Addr) -> Vec<u8> {
    let mut k = Vec::with_capacity(32 + 4);
    k.extend_from_slice(addr.peer.as_bytes());
    k.extend_from_slice(&addr.device.0.to_be_bytes());
    k
}

/// The device a [`session_key`] names.
pub(crate) fn session_addr(key: &[u8]) -> Option<Addr> {
    let (peer, device) = key.split_first_chunk::<32>()?;
    let device = DeviceId(u32::from_be_bytes(device.try_into().ok()?));
    device.is_valid().then(|| Addr::new(PeerId(*peer), device))
}

/// What follows a [`session_key`] in the key of a skipped message key kept
/// for that session: `session_key ‖ SKIPPED_ROW ‖ seq (BE)`.
const SKIPPED_ROW: u8 = 1;

/// Where the skipped message key numbered `seq` of the session with `addr`
/// is stored; a prefix scan by [`session_key`] yields the session's row
/// first, then its keys in the order they were set aside.
pub(crate) fn skipped_key_row(addr: Addr, seq: u64) -> Vec<u8> {
    let mut k = session_key(addr);
    k.push(SKIPPED_ROW);
    k.extend_from_slice(&seq.to_be_bytes());
    k
}

/// What a row of [`Table::Sessions`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionRow {
    /// The session with a device.
    Session(Addr),
    /// A skipped message key of that session, by its number.
    SkippedKey(Addr, u64),
}

/// Tells a row of [`Table::Sessions`] by its key; `None` for a key no
/// release writes.
pub(crate) fn session_row(key: &[u8]) -> Option<SessionRow> {
    let (session, rest) = key.split_at_checked(PeerId::LEN + 4)?;
    let addr = session_addr(session)?;
    match rest.split_first() {
        None => Some(SessionRow::Session(addr)),
        Some((&SKIPPED_ROW, seq)) => {
            let seq = u64::from_be_bytes(seq.try_into().ok()?);
            Some(SessionRow::SkippedKey(addr, seq))
        }
        Some(_) => None,
    }
}

pub fn message_key(peer: &PeerId, sent_at_ms: u64, msg_id: &MsgId) -> Vec<u8> {
    let mut k = Vec::with_capacity(32 + 8 + 16);
    k.extend_from_slice(peer.as_bytes());
    k.extend_from_slice(&sent_at_ms.to_be_bytes());
    k.extend_from_slice(msg_id.as_bytes());
    k
}

/// The first and the last [`message_key`] a conversation can have, both
/// inclusive: every message with `peer` lies between them, whatever its
/// date, so the range lists or deletes the whole conversation.
pub fn peer_messages(peer: &PeerId) -> (Vec<u8>, Vec<u8>) {
    (
        message_key(peer, 0, &MsgId([0; MsgId::LEN])),
        message_key(peer, u64::MAX, &MsgId([u8::MAX; MsgId::LEN])),
    )
}

/// The first and the last [`message_key`] of messages with `peer` sent
/// within `window_ms` of `at_ms`, both inclusive: where to look for one
/// message whose time is known only roughly.
pub fn message_window(peer: &PeerId, at_ms: u64, window_ms: u64) -> (Vec<u8>, Vec<u8>) {
    (
        message_key(
            peer,
            at_ms.saturating_sub(window_ms),
            &MsgId([0; MsgId::LEN]),
        ),
        message_key(
            peer,
            at_ms.saturating_add(window_ms),
            &MsgId([u8::MAX; MsgId::LEN]),
        ),
    )
}

/// How long every [`message_key`] is: peer, date, message id.
pub const MESSAGE_KEY_LEN: usize = PeerId::LEN + 8 + MsgId::LEN;

/// The id of the message stored under `key`, which ends every
/// [`message_key`]; `None` for a key of another length. The message's
/// status row is keyed by it.
pub fn message_id(key: &[u8]) -> Option<MsgId> {
    if key.len() != MESSAGE_KEY_LEN {
        return None;
    }
    let (_, id) = key.split_last_chunk::<{ MsgId::LEN }>()?;
    Some(MsgId(*id))
}

/// What deleting one stored message takes, for drivers to carry out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageDeletion {
    /// Its row, its status and a voice or video note's key: deleted in one
    /// batch.
    pub ops: Vec<StoreOp>,
    /// A voice or video note: its sealed copy goes too.
    pub note: Option<FileId>,
    /// A file sent or received: the driver forgets where it was saved, and
    /// leaves the file itself where it is.
    pub file: Option<FileId>,
}

/// What deleting the message stored under `key` takes; `msg` is what the
/// row holds, `None` when it cannot be read. A row that cannot be read
/// still goes with its status, both found by the key alone; what its
/// content would name stays unknown.
pub fn message_deletion(key: &[u8], msg: Option<&StoredMessage>) -> MessageDeletion {
    let mut out = MessageDeletion {
        ops: vec![StoreOp::Delete {
            table: Table::Messages,
            key: key.to_vec(),
        }],
        ..MessageDeletion::default()
    };
    if let Some(id) = message_id(key) {
        out.ops.push(StoreOp::Delete {
            table: Table::MessageStatus,
            key: id.to_vec(),
        });
    }
    let Some(msg) = msg else {
        return out;
    };
    match &msg.content {
        Content::File { file_id, kind, .. } if kind.is_media() => {
            out.ops.push(StoreOp::Delete {
                table: Table::Media,
                key: file_id.to_vec(),
            });
            out.note = Some(*file_id);
        }
        Content::File { file_id, .. } => out.file = Some(*file_id),
        Content::Text { .. } => {}
    }
    out
}

/// At-rest encryption for every record the core persists. Records are bound
/// to their `(table, key)` through the AEAD associated data, so a storage
/// attacker cannot swap values between keys.
#[derive(Clone)]
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

    /// Decrypts a message record together with the row of its latest
    /// status, which overrides the status kept with the message; a status
    /// row that cannot be read is passed over.
    pub fn open_message_with_status(
        &self,
        key: &[u8],
        sealed: &[u8],
        status: Option<impl AsRef<[u8]>>,
    ) -> Result<StoredMessage, CoreError> {
        let mut msg = self.open_message(key, sealed)?;
        if let Some(status) = status.and_then(|raw| {
            self.open::<MessageStatus>(Table::MessageStatus, msg.msg_id.as_bytes(), raw.as_ref())
                .ok()
        }) {
            msg.status = status;
        }
        Ok(msg)
    }

    /// Decrypts the playback key of a sealed media file.
    pub fn open_media(&self, file_id: &FileId, sealed: &[u8]) -> Result<MediaKey, CoreError> {
        self.open(Table::Media, file_id.as_bytes(), sealed)
    }

    /// A contact's names from its sealed session row (keyed by the peer id).
    /// Drivers list contacts without restoring sessions.
    pub fn open_contact(&self, peer_key: &[u8], sealed: &[u8]) -> Result<ContactInfo, CoreError> {
        self.open::<crate::peer::PeerRecord>(Table::Peers, peer_key, sealed)
            .map(|record| ContactInfo {
                alias: record.alias,
                name: record.name,
                request: record.request,
                blocked: record.blocked,
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

    fn stored(content: Content) -> StoredMessage {
        StoredMessage {
            msg_id: MsgId([4; 16]),
            peer: PeerId([5; 32]),
            outgoing: false,
            sent_at_ms: 6,
            status: MessageStatus::Delivered,
            content,
        }
    }

    fn deleted(d: &MessageDeletion) -> Vec<(Table, Vec<u8>)> {
        d.ops
            .iter()
            .map(|op| match op {
                StoreOp::Delete { table, key } => (*table, key.clone()),
                StoreOp::Put { .. } => panic!("a deletion puts nothing"),
            })
            .collect()
    }

    #[test]
    fn deleting_a_message_takes_its_status_and_its_note_but_only_names_a_file() {
        use crate::api::MediaKind;
        let file = |kind| Content::File {
            file_id: FileId([3; 16]),
            name: "f".into(),
            mime: "x/y".into(),
            size: 1,
            kind,
        };
        let key = message_key(&PeerId([5; 32]), 6, &MsgId([4; 16]));
        let row = (Table::Messages, key.clone());
        let status = (Table::MessageStatus, vec![4; 16]);

        let text = stored(Content::Text {
            text: "hi".into(),
            reply_to: None,
        });
        let d = message_deletion(&key, Some(&text));
        assert_eq!(deleted(&d), [row.clone(), status.clone()]);
        assert_eq!((d.note, d.file), (None, None));

        let voice = stored(file(MediaKind::Voice {
            duration_ms: 1,
            waveform: Vec::new(),
        }));
        let d = message_deletion(&key, Some(&voice));
        let note = (Table::Media, vec![3; 16]);
        assert_eq!(deleted(&d), [row.clone(), status.clone(), note]);
        assert_eq!((d.note, d.file), (Some(FileId([3; 16])), None));

        let d = message_deletion(&key, Some(&stored(file(MediaKind::File))));
        assert_eq!(deleted(&d), [row.clone(), status.clone()]);
        assert_eq!((d.note, d.file), (None, Some(FileId([3; 16]))));

        let d = message_deletion(&key, None);
        assert_eq!(deleted(&d), [row, status], "found by the key alone");
        assert_eq!((d.note, d.file), (None, None));
    }

    #[test]
    fn a_stored_status_overrides_the_one_kept_with_the_message() {
        let v = Vault::new([1; 32]);
        let msg = stored(Content::Text {
            text: "hi".into(),
            reply_to: None,
        });
        let key = message_key(&msg.peer, msg.sent_at_ms, &msg.msg_id);
        let sealed = v.seal(Table::Messages, &key, &msg, &mut OsRng);
        let read = v.seal(
            Table::MessageStatus,
            msg.msg_id.as_bytes(),
            &MessageStatus::Read,
            &mut OsRng,
        );
        let open = |status: Option<&[u8]>| {
            v.open_message_with_status(&key, &sealed, status)
                .unwrap()
                .status
        };
        assert_eq!(open(None), MessageStatus::Delivered);
        assert_eq!(open(Some(&read)), MessageStatus::Read);
        // Another message's status row does not apply to this one.
        let other = v.seal(
            Table::MessageStatus,
            &[9; 16],
            &MessageStatus::Read,
            &mut OsRng,
        );
        assert_eq!(open(Some(&other)), MessageStatus::Delivered);
        v.open_message_with_status(&key[1..], &sealed, Some(&read))
            .unwrap_err();
    }

    #[test]
    fn a_conversation_lies_within_its_range_and_no_other_does() {
        let p = PeerId([7; 32]);
        let (first, last) = peer_messages(&p);
        for (at, id) in [(0, [0; 16]), (1, [9; 16]), (u64::MAX, [0xFF; 16])] {
            let key = message_key(&p, at, &MsgId(id));
            assert!(first <= key && key <= last, "{at}");
        }
        let mut before = [7; 32];
        before[31] = 6;
        let mut after = [7; 32];
        after[31] = 8;
        for other in [PeerId(before), PeerId(after)] {
            for (at, id) in [(0, [0; 16]), (u64::MAX, [0xFF; 16])] {
                let key = message_key(&other, at, &MsgId(id));
                assert!(key < first || key > last);
            }
        }
    }

    #[test]
    fn a_message_window_holds_both_of_its_ends_and_nothing_past_them() {
        let p = PeerId([7; 32]);
        let inside = |(first, last): &(Vec<u8>, Vec<u8>), at, id| {
            let key = message_key(&p, at, &MsgId(id));
            *first <= key && key <= *last
        };
        let window = message_window(&p, 1_000, 10);
        for (at, id) in [(990, [0; 16]), (1_000, [9; 16]), (1_010, [0xFF; 16])] {
            assert!(inside(&window, at, id), "{at}");
        }
        for (at, id) in [(989, [0xFF; 16]), (1_011, [0; 16])] {
            assert!(!inside(&window, at, id), "{at}");
        }
        assert!(inside(&message_window(&p, 5, 10), 0, [0; 16]));
        assert!(inside(
            &message_window(&p, u64::MAX - 5, 10),
            u64::MAX,
            [0xFF; 16]
        ));
    }

    #[test]
    fn a_status_row_is_keyed_by_the_id_ending_its_message_key() {
        let key = message_key(&PeerId([1; 32]), 7, &MsgId([9; 16]));
        assert_eq!(message_id(&key), Some(MsgId([9; 16])));
        for len in [0, 16, MESSAGE_KEY_LEN - 1, MESSAGE_KEY_LEN + 1] {
            assert_eq!(message_id(&vec![9; len]), None, "{len}");
        }
        let d = message_deletion(&[9; 16], None);
        assert_eq!(d.ops.len(), 1, "no status row for a key that names none");
    }

    #[test]
    fn session_rows_are_told_by_their_key() {
        let addr = Addr::new(PeerId([7; 32]), DeviceId(3));
        let session = session_key(addr);
        assert_eq!(session_row(&session), Some(SessionRow::Session(addr)));
        let key = skipped_key_row(addr, 300);
        assert!(key.starts_with(&session));
        assert_eq!(session_row(&key), Some(SessionRow::SkippedKey(addr, 300)));
        assert!(
            session < skipped_key_row(addr, 0)
                && skipped_key_row(addr, 2) < skipped_key_row(addr, 256),
            "the session first, then its keys in the order set aside"
        );
        let longer = [&key[..], &[0]].concat();
        let unknown = [&session[..], &[9; 9]].concat();
        let no_device = session_key(Addr::new(PeerId([7; 32]), DeviceId(0)));
        for other in [
            &key[..key.len() - 1],
            &longer,
            &unknown,
            &session[..session.len() - 1],
            &no_device,
        ] {
            assert_eq!(session_row(other), None, "{other:?}");
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
