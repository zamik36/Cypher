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
/// Parts in one marked state reply: at most 51,200 contacts, while each
/// received index still needs its own tracking entry.
pub const MAX_STATE_PARTS: u32 = 1024;
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

/// How one device can be reached, as it tells its contacts, for `epoch`
/// and the next, so a contact keeps reaching it across the turn of the week
/// without waiting to hear again: the inbox it reads, and the access token
/// that lets a message be delivered to it sealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxAd {
    pub epoch: u32,
    pub ids: [[u8; 32]; 2],
    pub access: [[u8; 32]; 2],
}

impl InboxAd {
    /// The inbox to write to in `epoch`.
    pub fn at(&self, epoch: u32) -> [u8; 32] {
        self.pick(epoch, self.ids)
    }

    /// The access token to deliver with in `epoch`.
    pub fn token_at(&self, epoch: u32) -> [u8; 32] {
        self.pick(epoch, self.access)
    }

    /// The first of `pair` up to the announced epoch, the second after.
    fn pick(&self, epoch: u32, [this, next]: [[u8; 32]; 2]) -> [u8; 32] {
        if epoch > self.epoch { next } else { this }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Body {
    Hello {
        /// The inbox an older peer writes to: the first of `ad`.
        inbox: [u8; 32],
        /// The name the sender goes by. Appended last: an older peer reads
        /// `inbox` and takes the rest for padding, and from an older peer
        /// this decodes from the zero padding as `None`.
        name: Option<String>,
        /// The invite the sender joined by, so the host knows it asked for
        /// this contact. Appended after `name`, the same way.
        via: Option<String>,
        /// The inboxes the sender's device reads. Appended after `via`; a
        /// `Hello` without it names an inbox no one reads any more.
        ad: Option<InboxAd>,
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
    /// the inboxes of each device (`device id`, announcement).
    Devices {
        list: Vec<u8>,
        /// Inboxes no one reads any more; left empty.
        inboxes: Vec<(u32, [u8; 32])>,
        /// Appended last, like the fields of `Hello`.
        ads: Option<Vec<(u32, InboxAd)>>,
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
    /// A device new to the identity asks the others what they know. The
    /// optional request id is appended for older readers to ignore as padding.
    StateRequest { request: Option<MsgId> },
    /// Part of what this device knows, for a new one.
    State {
        profile: Option<String>,
        contacts: Vec<ContactState>,
        links: Vec<(String, u64)>,
        /// Which part of one complete reply this is. Appended last: an older
        /// peer reads `None`, and an older reader ignores it as padding.
        part: Option<StatePart>,
    },
    /// The message history, for a device just linked: an archive
    /// ([`crate::history`]) sent as a file.
    History { desc: FileDesc },
}

/// One part of a complete state reply. Parts may arrive in any order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatePart {
    /// The same for every part of one reply, different for a retry's reply.
    pub id: MsgId,
    pub index: u32,
    pub total: u32,
    /// The request this reply answers. Appended last, for the same padding
    /// compatibility as the optional fields on `State` and `StateRequest`.
    pub request: Option<MsgId>,
}

impl StatePart {
    pub(crate) fn valid(self) -> bool {
        self.total > 0 && self.total <= MAX_STATE_PARTS && self.index < self.total
    }
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
            Self::Removed { .. } | Self::StateRequest { .. } => true,
            Self::Profile { name } => name
                .as_ref()
                .is_none_or(|n| n.len() <= MAX_PROFILE_NAME_LEN),
            Self::Link { code, .. } => code.len() <= MAX_LINK_LEN,
            Self::State {
                profile,
                contacts,
                links,
                part,
            } => {
                profile
                    .as_ref()
                    .is_none_or(|n| n.len() <= MAX_PROFILE_NAME_LEN)
                    && contacts.len() <= MAX_STATE_CONTACTS
                    && contacts.iter().all(ContactState::valid)
                    && links.len() <= MAX_STATE_LINKS
                    && links.iter().all(|(code, _)| code.len() <= MAX_LINK_LEN)
                    && part.is_none_or(StatePart::valid)
            }
            Self::History { desc } => desc.inline.is_none() && desc.validate().is_ok(),
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
            Body::Devices { list, inboxes, ads } => {
                list.len() <= cypher_wire::MAX_DEVICE_LIST_LEN
                    && inboxes.len() <= cypher_types::MAX_DEVICES
                    && ads
                        .as_ref()
                        .is_none_or(|ads| ads.len() <= cypher_types::MAX_DEVICES)
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
                ad: None,
            }
        );

        let ad = InboxAd {
            epoch: 9,
            ids: [[7; 32], [8; 32]],
            access: [[5; 32], [6; 32]],
        };
        let new = Envelope {
            msg_id: MsgId([1; 16]),
            sent_at_ms: 5,
            body: Body::Hello {
                inbox: [7; 32],
                name: Some("Анна".into()),
                via: Some("abc".into()),
                ad: Some(ad),
            },
        }
        .encode();
        let (read, _) = postcard::take_from_bytes::<OldEnvelope>(&new).unwrap();
        assert!(matches!(read.body, OldBody::Hello { inbox } if inbox == [7; 32]));
        assert!(matches!(
            Envelope::decode(&new).unwrap().body,
            Body::Hello { name: Some(n), ad: Some(a), .. } if n == "Анна" && a == ad
        ));

        let long = Envelope {
            msg_id: MsgId([1; 16]),
            sent_at_ms: 5,
            body: Body::Hello {
                inbox: [7; 32],
                name: Some("x".repeat(MAX_PROFILE_NAME_LEN + 1)),
                via: None,
                ad: None,
            },
        };
        Envelope::decode(&long.encode()).unwrap_err();
    }

    #[test]
    fn an_announcement_names_the_inbox_of_each_epoch() {
        let ad = InboxAd {
            epoch: 10,
            ids: [[1; 32], [2; 32]],
            access: [[3; 32], [4; 32]],
        };
        assert_eq!(ad.at(9), [1; 32], "a clock behind writes to the first");
        assert_eq!(ad.at(10), [1; 32]);
        assert_eq!(ad.at(11), [2; 32]);
        assert_eq!(
            ad.at(40),
            [2; 32],
            "an old announcement, the latest it names"
        );
        assert_eq!((ad.token_at(10), ad.token_at(11)), ([3; 32], [4; 32]));
    }

    /// `Devices` as it was before announcements, read by today's code.
    #[test]
    fn devices_from_older_peers_carry_no_announcements() {
        #[derive(Serialize)]
        enum OldBody {
            _Hello,
            _Text,
            _File,
            _FileCtl,
            _Receipt,
            Devices {
                list: Vec<u8>,
                inboxes: Vec<(u32, [u8; 32])>,
            },
        }
        #[derive(Serialize)]
        struct OldEnvelope {
            msg_id: MsgId,
            sent_at_ms: u64,
            body: OldBody,
        }
        let mut old = postcard::to_allocvec(&OldEnvelope {
            msg_id: MsgId([1; 16]),
            sent_at_ms: 5,
            body: OldBody::Devices {
                list: vec![1, 2],
                inboxes: vec![(1, [3; 32])],
            },
        })
        .unwrap();
        old.resize(padded_len(old.len()), 0);
        assert_eq!(
            Envelope::decode(&old).unwrap().body,
            Body::Devices {
                list: vec![1, 2],
                inboxes: vec![(1, [3; 32])],
                ads: None,
            }
        );
    }

    /// `State` before replies were split into marked parts. The variant
    /// ordinals and the three original fields are unchanged.
    #[derive(Serialize, Deserialize)]
    enum OldSyncBody {
        _Sent,
        _Read,
        _Contact,
        _Removed,
        _Profile,
        _Link,
        _StateRequest,
        State {
            profile: Option<String>,
            contacts: Vec<ContactState>,
            links: Vec<(String, u64)>,
        },
    }

    #[derive(Serialize, Deserialize)]
    enum OldSyncEnvelopeBody {
        _Hello,
        _Text,
        _File,
        _FileCtl,
        _Receipt,
        _Devices,
        Sync(OldSyncBody),
    }

    #[derive(Serialize, Deserialize)]
    struct OldSyncEnvelope {
        msg_id: MsgId,
        sent_at_ms: u64,
        body: OldSyncEnvelopeBody,
    }

    fn contact() -> ContactState {
        ContactState {
            peer: PeerId([4; 32]),
            identity_dh: [5; 32],
            alias: Some("Bob".into()),
            name: None,
            request: false,
            blocked: false,
            via: None,
            devices: None,
        }
    }

    #[test]
    fn state_parts_travel_both_ways_with_older_peers() {
        let contact = contact();
        let mut old = postcard::to_allocvec(&OldSyncEnvelope {
            msg_id: MsgId([1; 16]),
            sent_at_ms: 5,
            body: OldSyncEnvelopeBody::Sync(OldSyncBody::State {
                profile: Some("Alice".into()),
                contacts: vec![contact.clone()],
                links: vec![("invite".into(), 7)],
            }),
        })
        .unwrap();
        old.resize(padded_len(old.len()), 0);
        assert_eq!(
            Envelope::decode(&old).unwrap().body,
            Body::Sync(SyncBody::State {
                profile: Some("Alice".into()),
                contacts: vec![contact.clone()],
                links: vec![("invite".into(), 7)],
                part: None,
            })
        );

        let new = Envelope {
            msg_id: MsgId([1; 16]),
            sent_at_ms: 5,
            body: Body::Sync(SyncBody::State {
                profile: Some("Alice".into()),
                contacts: vec![contact.clone()],
                links: vec![("invite".into(), 7)],
                part: Some(StatePart {
                    id: MsgId([8; 16]),
                    index: 1,
                    total: 2,
                    request: Some(MsgId([7; 16])),
                }),
            }),
        }
        .encode();
        let (read, padding) = postcard::take_from_bytes::<OldSyncEnvelope>(&new).unwrap();
        let OldSyncEnvelopeBody::Sync(OldSyncBody::State {
            profile,
            contacts,
            links,
        }) = read.body
        else {
            panic!("the old reader sees its State variant");
        };
        assert_eq!(profile.as_deref(), Some("Alice"));
        assert_eq!(contacts, [contact]);
        assert_eq!(links, [("invite".into(), 7)]);
        assert!(!padding.is_empty(), "the new part is trailing padding");
    }

    /// The immediately previous wire shape: requests had no id, and marked
    /// parts had no request id. Both new optional fields must be padding to
    /// this reader and decode as `None` from this writer.
    #[derive(Serialize, Deserialize)]
    enum MarkedSyncBody {
        _Sent,
        _Read,
        _Contact,
        _Removed,
        _Profile,
        _Link,
        StateRequest,
        State {
            profile: Option<String>,
            contacts: Vec<ContactState>,
            links: Vec<(String, u64)>,
            part: Option<MarkedStatePart>,
        },
    }

    #[derive(Serialize, Deserialize)]
    struct MarkedStatePart {
        id: MsgId,
        index: u32,
        total: u32,
    }

    #[derive(Serialize, Deserialize)]
    enum MarkedEnvelopeBody {
        _Hello,
        _Text,
        _File,
        _FileCtl,
        _Receipt,
        _Devices,
        Sync(MarkedSyncBody),
    }

    #[derive(Serialize, Deserialize)]
    struct MarkedEnvelope {
        msg_id: MsgId,
        sent_at_ms: u64,
        body: MarkedEnvelopeBody,
    }

    fn padded<T: Serialize>(value: &T) -> Vec<u8> {
        let mut bytes = postcard::to_allocvec(value).unwrap();
        bytes.resize(padded_len(bytes.len()), 0);
        bytes
    }

    #[test]
    fn state_correlation_travels_both_ways_with_marked_peers() {
        let old_request = padded(&MarkedEnvelope {
            msg_id: MsgId([1; 16]),
            sent_at_ms: 5,
            body: MarkedEnvelopeBody::Sync(MarkedSyncBody::StateRequest),
        });
        assert_eq!(
            Envelope::decode(&old_request).unwrap().body,
            Body::Sync(SyncBody::StateRequest { request: None })
        );

        let request = MsgId([6; 16]);
        let new_request = Envelope {
            msg_id: MsgId([1; 16]),
            sent_at_ms: 5,
            body: Body::Sync(SyncBody::StateRequest {
                request: Some(request),
            }),
        }
        .encode();
        let (read, padding) = postcard::take_from_bytes::<MarkedEnvelope>(&new_request).unwrap();
        assert!(matches!(
            read.body,
            MarkedEnvelopeBody::Sync(MarkedSyncBody::StateRequest)
        ));
        assert!(!padding.is_empty(), "the request id is trailing padding");
    }

    #[test]
    fn old_marked_state_parts_decode_without_correlation() {
        let old_part = padded(&MarkedEnvelope {
            msg_id: MsgId([1; 16]),
            sent_at_ms: 5,
            body: MarkedEnvelopeBody::Sync(MarkedSyncBody::State {
                profile: None,
                contacts: vec![contact()],
                links: Vec::new(),
                part: Some(MarkedStatePart {
                    id: MsgId([8; 16]),
                    index: 1,
                    total: 2,
                }),
            }),
        });
        assert_eq!(
            Envelope::decode(&old_part).unwrap().body,
            Body::Sync(SyncBody::State {
                profile: None,
                contacts: vec![contact()],
                links: Vec::new(),
                part: Some(StatePart {
                    id: MsgId([8; 16]),
                    index: 1,
                    total: 2,
                    request: None,
                }),
            })
        );
    }

    #[test]
    fn new_marked_state_parts_keep_correlation_as_padding_for_old_peers() {
        let request = MsgId([6; 16]);
        let new_part = Envelope {
            msg_id: MsgId([1; 16]),
            sent_at_ms: 5,
            body: Body::Sync(SyncBody::State {
                profile: None,
                contacts: vec![contact()],
                links: Vec::new(),
                part: Some(StatePart {
                    id: MsgId([8; 16]),
                    index: 1,
                    total: 2,
                    request: Some(request),
                }),
            }),
        }
        .encode();
        let (read, padding) = postcard::take_from_bytes::<MarkedEnvelope>(&new_part).unwrap();
        let MarkedEnvelopeBody::Sync(MarkedSyncBody::State {
            contacts,
            part: Some(part),
            ..
        }) = read.body
        else {
            panic!("the old reader sees its marked State variant");
        };
        assert_eq!(contacts, [contact()]);
        assert_eq!(part.id, MsgId([8; 16]));
        assert_eq!((part.index, part.total), (1, 2));
        assert!(
            !padding.is_empty(),
            "the correlation id is trailing padding"
        );
    }

    #[test]
    fn state_part_numbers_are_validated() {
        assert!(
            StatePart {
                id: MsgId([8; 16]),
                index: MAX_STATE_PARTS - 1,
                total: MAX_STATE_PARTS,
                request: None,
            }
            .valid()
        );
        for part in [
            StatePart {
                id: MsgId([8; 16]),
                index: 0,
                total: 0,
                request: None,
            },
            StatePart {
                id: MsgId([8; 16]),
                index: 2,
                total: 2,
                request: None,
            },
            StatePart {
                id: MsgId([8; 16]),
                index: 0,
                total: MAX_STATE_PARTS + 1,
                request: None,
            },
        ] {
            let encoded = Envelope {
                msg_id: MsgId([1; 16]),
                sent_at_ms: 5,
                body: Body::Sync(SyncBody::State {
                    profile: None,
                    contacts: Vec::new(),
                    links: Vec::new(),
                    part: Some(part),
                }),
            }
            .encode();
            Envelope::decode(&encoded).unwrap_err();
        }
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
