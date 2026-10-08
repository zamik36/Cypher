//! End-to-end message envelope: everything here is encrypted by the ratchet
//! and invisible to the server.

use cypher_types::{FileId, MsgId, PeerId};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::CoreError;
use crate::api::{Content, MediaKind};

pub const MAX_TEXT_LEN: usize = 16 * 1024;
pub const MAX_INLINE_LEN: usize = 32 * 1024;
pub const MAX_NAME_LEN: usize = 255;
/// A profile name in bytes; the core keeps at most 64 characters of it.
pub const MAX_PROFILE_NAME_LEN: usize = 256;
/// An invite code in a `Hello`.
pub const MAX_LINK_LEN: usize = 64;
pub const MAX_MIME_LEN: usize = 127;
pub const MAX_WAVEFORM_LEN: usize = 128;
pub const MAX_POSTER_LEN: usize = 16 * 1024;
pub const MAX_RECEIPT_IDS: usize = 512;
/// Contacts in one `SyncBody::State`, so it fits an inbox item.
pub const MAX_STATE_CONTACTS: usize = 50;
/// Invites in one `SyncBody::State`.
pub const MAX_STATE_LINKS: usize = 32;
/// A contact's name as this user gave it, in bytes.
const MAX_ALIAS_LEN: usize = 256;
pub const MAX_FILE_SIZE: u64 = 64 << 30;
pub const MAX_CHUNK_SIZE: u32 = 1 << 20;
pub const FILE_CHUNK_SIZE: u32 = 256 * 1024;
pub const MEDIA_CHUNK_SIZE: u32 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub msg_id: MsgId,
    pub sent_at_ms: u64,
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Body {
    Hello {
        inbox: [u8; 32],
        /// The name the sender goes by. Appended last: an older peer reads
        /// `inbox` and takes the rest for padding, and from an older peer
        /// this decodes from the zero padding as `None`.
        name: Option<String>,
        /// The invite the sender joined by, so the host knows it asked for
        /// this contact. Appended after `name`, the same way.
        via: Option<String>,
    },
    Text {
        text: String,
        reply_to: Option<MsgId>,
    },
    File {
        desc: FileDesc,
        kind: MediaKind,
    },
    FileCtl(FileCtl),
    Receipt {
        kind: ReceiptKind,
        ids: Vec<MsgId>,
    },
    /// The sender's identity lists these devices now: its signed list, and
    /// the inbox of each device (`device id`, inbox id).
    Devices {
        list: Vec<u8>,
        inboxes: Vec<(u32, [u8; 32])>,
    },
    /// Between devices of one identity only.
    Sync(SyncBody),
}

/// What one device of an identity tells its other devices, so they show
/// the same conversations. Accepted only from the same identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncBody {
    /// A message this device sent to a contact.
    Sent {
        peer: PeerId,
        msg_id: MsgId,
        sent_at_ms: u64,
        content: Content,
    },
    /// A contact's messages read on this device.
    Read { peer: PeerId, ids: Vec<MsgId> },
    /// A contact as this device keeps it now.
    Contact(ContactState),
    /// A contact forgotten on this device.
    Removed { peer: PeerId },
    /// The name this user goes by.
    Profile { name: Option<String> },
    /// An invite made on this device, and when.
    Link { code: String, at_ms: u64 },
    /// A device new to the identity asks the others what they know.
    StateRequest,
    /// Part of what this device knows, for a new one.
    State {
        profile: Option<String>,
        contacts: Vec<ContactState>,
        links: Vec<(String, u64)>,
    },
}

/// A contact as one device keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactState {
    pub peer: PeerId,
    pub identity_dh: [u8; 32],
    pub alias: Option<String>,
    pub name: Option<String>,
    pub request: bool,
    pub blocked: bool,
    pub via: Option<String>,
    /// Their signed device list, if known.
    pub devices: Option<Vec<u8>>,
}

impl ContactState {
    fn valid(&self) -> bool {
        self.alias.as_ref().is_none_or(|a| a.len() <= MAX_ALIAS_LEN)
            && self
                .name
                .as_ref()
                .is_none_or(|n| n.len() <= MAX_PROFILE_NAME_LEN)
            && self.via.as_ref().is_none_or(|v| v.len() <= MAX_LINK_LEN)
            && self
                .devices
                .as_ref()
                .is_none_or(|d| d.len() <= cypher_wire::MAX_DEVICE_LIST_LEN)
    }
}

impl SyncBody {
    fn valid(&self) -> bool {
        match self {
            Self::Sent { content, .. } => match content {
                Content::Text { text, .. } => text.len() <= MAX_TEXT_LEN,
                Content::File {
                    name, mime, kind, ..
                } => name.len() <= MAX_NAME_LEN && mime.len() <= MAX_MIME_LEN && kind_valid(kind),
            },
            Self::Read { ids, .. } => ids.len() <= MAX_RECEIPT_IDS,
            Self::Contact(contact) => contact.valid(),
            Self::Removed { .. } | Self::StateRequest => true,
            Self::Profile { name } => name
                .as_ref()
                .is_none_or(|n| n.len() <= MAX_PROFILE_NAME_LEN),
            Self::Link { code, .. } => code.len() <= MAX_LINK_LEN,
            Self::State {
                profile,
                contacts,
                links,
            } => {
                profile
                    .as_ref()
                    .is_none_or(|n| n.len() <= MAX_PROFILE_NAME_LEN)
                    && contacts.len() <= MAX_STATE_CONTACTS
                    && contacts.iter().all(ContactState::valid)
                    && links.len() <= MAX_STATE_LINKS
                    && links.iter().all(|(code, _)| code.len() <= MAX_LINK_LEN)
            }
        }
    }
}

fn kind_valid(kind: &MediaKind) -> bool {
    match kind {
        MediaKind::File => true,
        MediaKind::Voice { waveform, .. } => waveform.len() <= MAX_WAVEFORM_LEN,
        MediaKind::VideoNote { poster, .. } => poster.len() <= MAX_POSTER_LEN,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReceiptKind {
    Delivered,
    Read,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileCtl {
    /// Start or resume: `have` is a bitmap of chunks already stored.
    Accept {
        file_id: FileId,
        have: Vec<u8>,
    },
    Cancel {
        file_id: FileId,
    },
}

/// Describes a transfer. The key never leaves the encrypted envelope.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDesc {
    pub file_id: FileId,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub chunk_size: u32,
    pub key: [u8; 32],
    pub inline: Option<Vec<u8>>,
}

impl Drop for FileDesc {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl std::fmt::Debug for FileDesc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileDesc")
            .field("file_id", &self.file_id)
            .field("size", &self.size)
            .field("chunk_size", &self.chunk_size)
            .finish_non_exhaustive()
    }
}

impl FileDesc {
    pub fn chunk_count(&self) -> u32 {
        chunk_count(self.size, self.chunk_size)
    }

    /// Plaintext length of chunk `index`.
    pub fn chunk_len(&self, index: u32) -> u32 {
        let start = u64::from(index) * u64::from(self.chunk_size);
        let rest = self.size.saturating_sub(start);
        u32::try_from(rest).map_or(self.chunk_size, |rest| rest.min(self.chunk_size))
    }

    fn validate(&self) -> Result<(), CoreError> {
        let ok = self.name.len() <= MAX_NAME_LEN
            && self.mime.len() <= MAX_MIME_LEN
            && self.size <= MAX_FILE_SIZE
            && (1..=MAX_CHUNK_SIZE).contains(&self.chunk_size)
            && u64::from(self.chunk_count()) * u64::from(self.chunk_size) >= self.size
            && self.inline.as_ref().is_none_or(|b| {
                b.len() <= MAX_INLINE_LEN && b.len() as u64 == self.size && self.chunk_count() == 1
            });
        if ok { Ok(()) } else { Err(CoreError::Invalid) }
    }
}

pub fn chunk_count(size: u64, chunk_size: u32) -> u32 {
    u32::try_from(size.div_ceil(u64::from(chunk_size)).max(1)).unwrap_or(u32::MAX)
}

impl Envelope {
    #[expect(
        clippy::expect_used,
        reason = "postcard serialization into a Vec cannot fail for envelopes"
    )]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = postcard::to_allocvec(self).expect("in-memory serialization");
        buf.resize(padded_len(buf.len()), 0);
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, CoreError> {
        let (env, _padding) =
            postcard::take_from_bytes::<Self>(buf).map_err(|_| CoreError::Invalid)?;
        env.validate()?;
        Ok(env)
    }

    fn validate(&self) -> Result<(), CoreError> {
        let ok = match &self.body {
            Body::Hello { name, via, .. } => {
                name.as_ref()
                    .is_none_or(|n| n.len() <= MAX_PROFILE_NAME_LEN)
                    && via.as_ref().is_none_or(|v| v.len() <= MAX_LINK_LEN)
            }
            Body::FileCtl(FileCtl::Cancel { .. }) => true,
            Body::Text { text, .. } => text.len() <= MAX_TEXT_LEN,
            Body::File { desc, kind } => {
                desc.validate()?;
                kind_valid(kind)
            }
            Body::FileCtl(FileCtl::Accept { have, .. }) => {
                have.len() <= (u32::MAX as usize).div_ceil(8)
            }
            Body::Receipt { ids, .. } => ids.len() <= MAX_RECEIPT_IDS,
            Body::Devices { list, inboxes } => {
                list.len() <= cypher_wire::MAX_DEVICE_LIST_LEN
                    && inboxes.len() <= cypher_types::MAX_DEVICES
            }
            Body::Sync(sync) => sync.valid(),
        };
        if ok { Ok(()) } else { Err(CoreError::Invalid) }
    }
}

/// Hides the exact plaintext length: power-of-two buckets from 256 B to
/// 64 KiB, then 64 KiB granularity.
pub fn padded_len(n: usize) -> usize {
    const MIN: usize = 256;
    const STEP: usize = 64 * 1024;
    if n <= STEP {
        n.max(MIN).next_power_of_two()
    } else {
        n.div_ceil(STEP) * STEP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(size: u64, chunk_size: u32) -> FileDesc {
        FileDesc {
            file_id: FileId([1; 16]),
            name: "a.txt".into(),
            mime: "text/plain".into(),
            size,
            chunk_size,
            key: [2; 32],
            inline: None,
        }
    }

    /// `Hello` before it carried a name, as older clients encode it.
    #[derive(Serialize, Deserialize)]
    enum OldBody {
        Hello { inbox: [u8; 32] },
    }

    #[derive(Serialize, Deserialize)]
    struct OldEnvelope {
        msg_id: MsgId,
        sent_at_ms: u64,
        body: OldBody,
    }

    #[test]
    fn hello_names_travel_both_ways_with_older_peers() {
        let mut old = postcard::to_allocvec(&OldEnvelope {
            msg_id: MsgId([1; 16]),
            sent_at_ms: 5,
            body: OldBody::Hello { inbox: [7; 32] },
        })
        .unwrap();
        old.resize(padded_len(old.len()), 0);
        assert_eq!(
            Envelope::decode(&old).unwrap().body,
            Body::Hello {
                inbox: [7; 32],
                name: None,
                via: None,
            }
        );

        let new = Envelope {
            msg_id: MsgId([1; 16]),
            sent_at_ms: 5,
            body: Body::Hello {
                inbox: [7; 32],
                name: Some("Анна".into()),
                via: Some("abc".into()),
            },
        }
        .encode();
        let (read, _) = postcard::take_from_bytes::<OldEnvelope>(&new).unwrap();
        assert!(matches!(read.body, OldBody::Hello { inbox } if inbox == [7; 32]));
        assert!(matches!(
            Envelope::decode(&new).unwrap().body,
            Body::Hello { name: Some(n), .. } if n == "Анна"
        ));

        let long = Envelope {
            msg_id: MsgId([1; 16]),
            sent_at_ms: 5,
            body: Body::Hello {
                inbox: [7; 32],
                name: Some("x".repeat(MAX_PROFILE_NAME_LEN + 1)),
                via: None,
            },
        };
        Envelope::decode(&long.encode()).unwrap_err();
    }

    #[test]
    fn padding_buckets() {
        assert_eq!(padded_len(0), 256);
        assert_eq!(padded_len(256), 256);
        assert_eq!(padded_len(257), 512);
        assert_eq!(padded_len(65_536), 65_536);
        assert_eq!(padded_len(65_537), 131_072);
    }

    #[test]
    fn envelope_roundtrip_with_padding() {
        let env = Envelope {
            msg_id: MsgId([3; 16]),
            sent_at_ms: 1_700_000_000_000,
            body: Body::Text {
                text: "{\"json-like\":1}".into(),
                reply_to: None,
            },
        };
        let buf = env.encode();
        assert_eq!(buf.len(), 256);
        assert_eq!(Envelope::decode(&buf).unwrap(), env);
    }

    #[test]
    fn chunk_math() {
        let d = desc(10, 4);
        assert_eq!(d.chunk_count(), 3);
        assert_eq!((d.chunk_len(0), d.chunk_len(2)), (4, 2));
        assert_eq!(desc(0, 4).chunk_count(), 1);
        assert_eq!(desc(0, 4).chunk_len(0), 0);
    }

    #[test]
    fn hostile_descriptors_are_rejected() {
        let wrap = |desc: FileDesc| Envelope {
            msg_id: MsgId([0; 16]),
            sent_at_ms: 0,
            body: Body::File {
                desc,
                kind: MediaKind::File,
            },
        };
        Envelope::decode(&wrap(desc(MAX_FILE_SIZE + 1, 4)).encode()).unwrap_err();
        Envelope::decode(&wrap(desc(10, 0)).encode()).unwrap_err();
        Envelope::decode(&wrap(desc(10, MAX_CHUNK_SIZE + 1)).encode()).unwrap_err();
        let mut lying_inline = desc(10, 10);
        lying_inline.inline = Some(vec![0; 3]);
        Envelope::decode(&wrap(lying_inline).encode()).unwrap_err();
        let mut long_name = desc(10, 4);
        long_name.name = "x".repeat(MAX_NAME_LEN + 1);
        Envelope::decode(&wrap(long_name).encode()).unwrap_err();
        Envelope::decode(&[0xFF; 8]).unwrap_err();
    }
}
