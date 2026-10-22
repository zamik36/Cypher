use cypher_crypto::double_ratchet::SkippedKey;
use cypher_crypto::{DeviceList, InitHeader, Ratchet};
use cypher_types::DeviceId;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::CoreError;
use crate::envelope::InboxAd;

const MAX_REMEMBERED_INITS: usize = 16;
/// Longest name kept for anyone (a contact, or this user), in characters.
pub(crate) const MAX_NAME_CHARS: usize = 64;

/// What we know of a contact, whichever of their devices we talk to.
pub(crate) struct Peer {
    /// The contact's X25519 identity key: their devices share it, and
    /// sealed inbox items are addressed to it.
    pub identity_dh: [u8; 32],
    /// The name the user gave this contact; only ever stored on this device.
    pub alias: Option<String>,
    /// The name the contact goes by, as they last sent it.
    pub name: Option<String>,
    /// Started a session without one of our invites, not accepted yet: we
    /// tell them nothing (no `Hello`) until the user accepts.
    pub request: bool,
    /// Everything from them is dropped unread.
    pub blocked: bool,
    /// The invite we joined them by, told in our `Hello`.
    pub via: Option<String>,
    /// Their devices, as their identity last listed them; unknown for a
    /// contact who has not told us yet, taken as their first device only.
    pub devices: Option<DeviceList>,
    /// Inboxes of their devices, as they announced them: a device can be
    /// written to offline before its own `Hello` reached us.
    pub inboxes: Vec<(DeviceId, InboxAd)>,
}

/// One Double Ratchet session with one device of a contact.
pub(crate) struct Session {
    pub ratchet: Ratchet,
    /// Which line of sessions with the device this one continues (a
    /// session that replaces another keeps it): the rows kept beside the
    /// session carry it, so that rows a session long deleted left behind
    /// are never taken for this one's.
    pub lineage: u64,
    /// Initiator side: attached to every message until the responder has
    /// provably received one (any reply decrypts).
    pub pending_init: Option<InitHeader>,
    /// Responder side: ephemeral keys of inits already accepted, so repeated
    /// or replayed copies reuse the live session instead of resetting it.
    pub accepted_ephemerals: Vec<[u8; 32]>,
    /// That device's inboxes, as its `Hello` told.
    pub inbox: Option<InboxAd>,
    /// Whether our `Hello` went out on this session.
    pub hello_sent: bool,
    /// Until when messages to the device go as ourselves rather than
    /// sealed: its token was refused (until it greets again) or used up.
    /// In memory only.
    pub sealed_off_until: u64,
}

/// A name as someone typed it, made safe to show: control characters
/// dropped, trimmed, at most [`MAX_NAME_CHARS`]; empty means none.
pub(crate) fn clean_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_NAME_CHARS)
        .collect();
    let cleaned = cleaned.trim_end();
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

impl Peer {
    /// Someone not known before.
    pub(crate) fn new(identity_dh: [u8; 32]) -> Self {
        Self {
            identity_dh,
            alias: None,
            name: None,
            request: false,
            blocked: false,
            via: None,
            devices: None,
            inboxes: Vec::new(),
        }
    }

    /// The devices of theirs to write to.
    pub(crate) fn device_ids(&self) -> Vec<DeviceId> {
        self.devices
            .as_ref()
            .map_or_else(|| vec![DeviceId::FIRST], |list| list.devices().to_vec())
    }

    /// Whether `device` is one their identity lists (the first, while we
    /// know no list).
    pub(crate) fn lists(&self, device: DeviceId) -> bool {
        self.devices
            .as_ref()
            .map_or(device == DeviceId::FIRST, |list| list.contains(device))
    }

    /// The inboxes their `device` announced.
    pub(crate) fn inbox_of(&self, device: DeviceId) -> Option<InboxAd> {
        self.inboxes
            .iter()
            .find(|(d, _)| *d == device)
            .map(|(_, inbox)| *inbox)
    }

    pub(crate) fn to_record(&self) -> PeerRecord {
        PeerRecord {
            identity_dh: self.identity_dh,
            alias: self.alias.clone(),
            name: self.name.clone(),
            request: self.request,
            blocked: self.blocked,
            via: self.via.clone(),
            devices: self.devices.as_ref().map(DeviceList::encode),
            inboxes: self.inboxes.iter().map(|(d, i)| (d.0, *i)).collect(),
            legacy: None,
        }
    }

    pub(crate) fn from_record(r: &PeerRecord) -> Self {
        Self {
            identity_dh: r.identity_dh,
            alias: r.alias.clone(),
            name: r.name.clone(),
            request: r.request,
            blocked: r.blocked,
            via: r.via.clone(),
            // Signed, so a list that no longer verifies is simply unknown.
            devices: r
                .devices
                .as_deref()
                .and_then(|l| DeviceList::decode(l).ok()),
            inboxes: r
                .inboxes
                .iter()
                .map(|(d, i)| (DeviceId(*d), *i))
                .filter(|(d, _)| d.is_valid())
                .collect(),
        }
    }
}

impl Session {
    /// A session just made with one device, starting a new `lineage`.
    pub(crate) fn new(ratchet: Ratchet, pending_init: Option<InitHeader>, lineage: u64) -> Self {
        Self {
            ratchet,
            lineage,
            pending_init,
            accepted_ephemerals: Vec::new(),
            inbox: None,
            hello_sent: false,
            sealed_off_until: 0,
        }
    }

    /// Keeps what belongs to the device rather than the old session: its
    /// inbox, the inits seen from it, which of its messages were read, so
    /// that a copy of one is still known, and the line of sessions with it.
    /// The old session's skipped keys go. Our `Hello` goes out again.
    pub(crate) fn carry_over(&mut self, mut old: Self) {
        self.ratchet.inherit(&mut old.ratchet);
        self.lineage = old.lineage;
        self.inbox = old.inbox;
        self.accepted_ephemerals = old.accepted_ephemerals;
    }

    pub(crate) fn is_unconfirmed_initiator(&self) -> bool {
        self.pending_init.is_some()
    }

    pub(crate) fn remember_ephemeral(&mut self, ephemeral: [u8; 32]) {
        if self.accepted_ephemerals.len() >= MAX_REMEMBERED_INITS {
            self.accepted_ephemerals.remove(0);
        }
        self.accepted_ephemerals.push(ephemeral);
    }

    /// The session's row; the skipped keys numbered `spent` are deleted
    /// right after it is stored.
    pub(crate) fn to_record(&self, spent: Vec<u64>) -> SessionRecord {
        let mut init = Vec::new();
        if let Some(h) = &self.pending_init {
            h.encode(&mut init);
        }
        SessionRecord {
            ratchet: self.ratchet.to_bytes(),
            pending_init: init,
            accepted_ephemerals: self.accepted_ephemerals.clone(),
            inbox: self.inbox,
            hello_sent: self.hello_sent,
            lineage: self.lineage,
            spent,
            migrated: false,
        }
    }

    /// The session a row holds, without the skipped keys kept beside it;
    /// `now_ms` dates the keys a row from before they were kept apart
    /// holds inside.
    pub(crate) fn from_record(r: &SessionRecord, now_ms: u64) -> Result<Self, CoreError> {
        let pending_init = if r.pending_init.is_empty() {
            None
        } else {
            let (h, rest) =
                InitHeader::decode_prefix(&r.pending_init).map_err(|_| CoreError::Storage)?;
            if !rest.is_empty() {
                return Err(CoreError::Storage);
            }
            Some(h)
        };
        Ok(Self {
            ratchet: Ratchet::from_bytes(&r.ratchet, now_ms).map_err(|_| CoreError::Storage)?,
            lineage: r.lineage,
            pending_init,
            accepted_ephemerals: r.accepted_ephemerals.clone(),
            inbox: r.inbox,
            hello_sent: r.hello_sent,
            sealed_off_until: 0,
        })
    }
}

/// A contact as stored: who they are to the user. Their sessions are kept
/// apart, one per device ([`SessionRecord`]).
#[derive(Serialize, Deserialize)]
pub(crate) struct PeerRecord {
    identity_dh: [u8; 32],
    pub(crate) alias: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) request: bool,
    pub(crate) blocked: bool,
    via: Option<String>,
    devices: Option<Vec<u8>>,
    inboxes: Vec<(u32, InboxAd)>,
    /// The one session a record from before devices held, to be moved to
    /// its own row (as the contact's first device) on load.
    #[serde(skip)]
    pub(crate) legacy: Option<SessionRecord>,
}

impl crate::Record for PeerRecord {
    const VERSION: u8 = 7;

    /// Version 6 kept inboxes no one reads any more: the contact's devices
    /// announce their inboxes again. Version 5 knew nothing of the
    /// contact's devices. Up to version 4 a contact and its one session were
    /// stored together: the session comes out as `legacy`.
    fn upgrade(version: u8, body: &[u8]) -> Result<Self, CoreError> {
        if version == 6 {
            let v6: PeerRecordV6 = parse(body)?;
            return Ok(Self {
                identity_dh: v6.identity_dh,
                alias: v6.alias,
                name: v6.name,
                request: v6.request,
                blocked: v6.blocked,
                via: v6.via,
                devices: v6.devices,
                inboxes: Vec::new(),
                legacy: None,
            });
        }
        if version == 5 {
            let v5: PeerRecordV5 = parse(body)?;
            return Ok(Self {
                identity_dh: v5.identity_dh,
                alias: v5.alias,
                name: v5.name,
                request: v5.request,
                blocked: v5.blocked,
                via: v5.via,
                devices: None,
                inboxes: Vec::new(),
                legacy: None,
            });
        }
        let v4 = PeerRecordV4::upgrade(version, body)?;
        Ok(Self {
            identity_dh: v4.identity_dh,
            alias: v4.alias,
            name: v4.name,
            request: v4.request,
            blocked: v4.blocked,
            via: v4.via,
            devices: None,
            inboxes: Vec::new(),
            legacy: Some(SessionRecord {
                ratchet: v4.ratchet,
                pending_init: v4.pending_init,
                accepted_ephemerals: v4.accepted_ephemerals,
                inbox: None,
                hello_sent: v4.hello_sent,
                lineage: 0,
                spent: Vec::new(),
                migrated: true,
            }),
        })
    }
}

/// [`PeerRecord`] as version 6 stored it.
#[derive(Deserialize)]
struct PeerRecordV6 {
    identity_dh: [u8; 32],
    alias: Option<String>,
    name: Option<String>,
    request: bool,
    blocked: bool,
    via: Option<String>,
    devices: Option<Vec<u8>>,
}

/// One session with one device of a contact, as stored. Its skipped
/// message keys are stored beside it, one row each ([`SkippedKeyRecord`]).
#[derive(Serialize, Deserialize)]
pub(crate) struct SessionRecord {
    #[serde(with = "zeroizing_bytes")]
    ratchet: Zeroizing<Vec<u8>>,
    pending_init: Vec<u8>,
    accepted_ephemerals: Vec<[u8; 32]>,
    inbox: Option<InboxAd>,
    hello_sent: bool,
    /// See [`Session::lineage`].
    lineage: u64,
    /// Skipped keys this row no longer counts, whose rows are deleted right
    /// after it is stored: should the app stop in between, the next start
    /// deletes them.
    pub(crate) spent: Vec<u64>,
    /// Read from a version that kept the skipped keys inside the ratchet:
    /// they come out of it, and no row beside it is the session's.
    #[serde(skip)]
    pub(crate) migrated: bool,
}

impl crate::Record for SessionRecord {
    const VERSION: u8 = 3;

    /// Version 2 kept the skipped keys inside the ratchet, and version 1
    /// also an inbox no one reads any more: the device announces its
    /// inboxes again.
    fn upgrade(version: u8, body: &[u8]) -> Result<Self, CoreError> {
        let v2 = match version {
            1 => {
                let v1: SessionRecordV1 = parse(body)?;
                SessionRecordV2 {
                    ratchet: v1.ratchet,
                    pending_init: v1.pending_init,
                    accepted_ephemerals: v1.accepted_ephemerals,
                    inbox: None,
                    hello_sent: v1.hello_sent,
                }
            }
            2 => parse(body)?,
            _ => return Err(CoreError::Storage),
        };
        Ok(Self {
            ratchet: v2.ratchet,
            pending_init: v2.pending_init,
            accepted_ephemerals: v2.accepted_ephemerals,
            inbox: v2.inbox,
            hello_sent: v2.hello_sent,
            lineage: 0,
            spent: Vec::new(),
            migrated: true,
        })
    }
}

/// [`SessionRecord`] as version 2 stored it.
#[derive(Deserialize)]
struct SessionRecordV2 {
    #[serde(with = "zeroizing_bytes")]
    ratchet: Zeroizing<Vec<u8>>,
    pending_init: Vec<u8>,
    accepted_ephemerals: Vec<[u8; 32]>,
    inbox: Option<InboxAd>,
    hello_sent: bool,
}

/// A skipped message key of a session, as stored in its own row
/// ([`crate::store::skipped_key_row`]), which tells its number.
#[derive(Serialize, Deserialize)]
pub(crate) struct SkippedKeyRecord {
    /// The [`Session::lineage`] of the session that set it aside.
    pub(crate) lineage: u64,
    dh: [u8; 32],
    n: u32,
    mk: [u8; 32],
    created_ms: u64,
}

impl SkippedKeyRecord {
    pub(crate) fn new(lineage: u64, key: &SkippedKey) -> Self {
        Self {
            lineage,
            dh: *key.dh(),
            n: key.n(),
            mk: *key.mk(),
            created_ms: key.created_ms(),
        }
    }

    /// The key, stored under `seq`.
    pub(crate) fn key(&self, seq: u64) -> SkippedKey {
        SkippedKey::new(seq, self.dh, self.n, self.mk, self.created_ms)
    }
}

impl Drop for SkippedKeyRecord {
    fn drop(&mut self) {
        self.mk.zeroize();
    }
}

impl crate::Record for SkippedKeyRecord {
    const VERSION: u8 = 1;
}

/// [`SessionRecord`] as version 1 stored it.
#[derive(Deserialize)]
struct SessionRecordV1 {
    #[serde(with = "zeroizing_bytes")]
    ratchet: Zeroizing<Vec<u8>>,
    pending_init: Vec<u8>,
    accepted_ephemerals: Vec<[u8; 32]>,
    #[expect(dead_code, reason = "an inbox no one reads any more")]
    inbox: Option<[u8; 32]>,
    hello_sent: bool,
}

/// [`PeerRecord`] as version 5 stored it.
#[derive(Deserialize)]
struct PeerRecordV5 {
    identity_dh: [u8; 32],
    alias: Option<String>,
    name: Option<String>,
    request: bool,
    blocked: bool,
    via: Option<String>,
}

/// [`PeerRecord`] as version 4 stored it, a contact and its session.
#[derive(Deserialize)]
struct PeerRecordV4 {
    #[serde(with = "zeroizing_bytes")]
    ratchet: Zeroizing<Vec<u8>>,
    pending_init: Vec<u8>,
    accepted_ephemerals: Vec<[u8; 32]>,
    identity_dh: [u8; 32],
    #[expect(dead_code, reason = "an inbox no one reads any more")]
    inbox: Option<[u8; 32]>,
    hello_sent: bool,
    alias: Option<String>,
    name: Option<String>,
    request: bool,
    blocked: bool,
    via: Option<String>,
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, CoreError> {
    postcard::from_bytes(body).map_err(|_| CoreError::Storage)
}

impl PeerRecordV4 {
    /// Version 1 had no names, version 2 only the user's own for the
    /// contact, version 3 no request or block state: everyone stored before
    /// is a contact the user already has.
    fn upgrade(version: u8, body: &[u8]) -> Result<Self, CoreError> {
        if version == 4 {
            return parse(body);
        }
        if version == 3 {
            let v3: PeerRecordV3 = parse(body)?;
            return Ok(Self {
                ratchet: v3.ratchet,
                pending_init: v3.pending_init,
                accepted_ephemerals: v3.accepted_ephemerals,
                identity_dh: v3.identity_dh,
                inbox: v3.inbox,
                hello_sent: v3.hello_sent,
                alias: v3.alias,
                name: v3.name,
                request: false,
                blocked: false,
                via: None,
            });
        }
        let v2: PeerRecordV2 = match version {
            1 => {
                let v1: PeerRecordV1 = parse(body)?;
                PeerRecordV2 {
                    ratchet: v1.ratchet,
                    pending_init: v1.pending_init,
                    accepted_ephemerals: v1.accepted_ephemerals,
                    identity_dh: v1.identity_dh,
                    inbox: v1.inbox,
                    hello_sent: v1.hello_sent,
                    alias: None,
                }
            }
            2 => parse(body)?,
            _ => return Err(CoreError::Storage),
        };
        Ok(Self {
            ratchet: v2.ratchet,
            pending_init: v2.pending_init,
            accepted_ephemerals: v2.accepted_ephemerals,
            identity_dh: v2.identity_dh,
            inbox: v2.inbox,
            hello_sent: v2.hello_sent,
            alias: v2.alias,
            name: None,
            request: false,
            blocked: false,
            via: None,
        })
    }
}

/// [`PeerRecord`] as version 3 stored it.
#[derive(Deserialize)]
struct PeerRecordV3 {
    #[serde(with = "zeroizing_bytes")]
    ratchet: Zeroizing<Vec<u8>>,
    pending_init: Vec<u8>,
    accepted_ephemerals: Vec<[u8; 32]>,
    identity_dh: [u8; 32],
    inbox: Option<[u8; 32]>,
    hello_sent: bool,
    alias: Option<String>,
    name: Option<String>,
}

/// Invites this user made, with when: whoever joins by one is a contact.
#[derive(Serialize, Deserialize, Default)]
pub(crate) struct OwnLinks {
    pub links: Vec<(String, u64)>,
}

impl crate::Record for OwnLinks {
    const VERSION: u8 = 1;
}

/// [`PeerRecord`] as version 2 stored it.
#[derive(Deserialize)]
struct PeerRecordV2 {
    #[serde(with = "zeroizing_bytes")]
    ratchet: Zeroizing<Vec<u8>>,
    pending_init: Vec<u8>,
    accepted_ephemerals: Vec<[u8; 32]>,
    identity_dh: [u8; 32],
    inbox: Option<[u8; 32]>,
    hello_sent: bool,
    alias: Option<String>,
}

/// The name this user goes by, kept in `meta`.
#[derive(Serialize, Deserialize, Default)]
pub(crate) struct ProfileRecord {
    pub name: Option<String>,
}

impl crate::Record for ProfileRecord {
    const VERSION: u8 = 1;
}

/// [`PeerRecord`] as version 1 stored it.
#[derive(Deserialize)]
struct PeerRecordV1 {
    #[serde(with = "zeroizing_bytes")]
    ratchet: Zeroizing<Vec<u8>>,
    pending_init: Vec<u8>,
    accepted_ephemerals: Vec<[u8; 32]>,
    identity_dh: [u8; 32],
    inbox: Option<[u8; 32]>,
    hello_sent: bool,
}

mod zeroizing_bytes {
    use serde::{Deserialize, Deserializer, Serializer};
    use zeroize::Zeroizing;

    pub(super) fn serialize<S: Serializer>(
        v: &Zeroizing<Vec<u8>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(v)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Zeroizing<Vec<u8>>, D::Error> {
        Vec::<u8>::deserialize(d).map(Zeroizing::new)
    }
}

#[cfg(test)]
mod tests {
    use rand::rngs::OsRng;
    use serde::Serialize;

    use super::*;
    use crate::{Table, Vault};

    #[test]
    fn names_are_cleaned_and_bounded() {
        assert_eq!(clean_name("  Anna  "), Some("Anna".into()));
        assert_eq!(clean_name("An\u{0}na\n"), Some("Anna".into()));
        assert_eq!(clean_name(" \t "), None);
        let long = "я".repeat(MAX_NAME_CHARS + 10);
        assert_eq!(
            clean_name(&long).map(|a| a.chars().count()),
            Some(MAX_NAME_CHARS)
        );
    }

    /// Sessions saved before contact names existed load without a name.
    #[test]
    fn version_1_records_load_without_a_name() {
        #[derive(Serialize, Deserialize)]
        struct V1 {
            ratchet: Vec<u8>,
            pending_init: Vec<u8>,
            accepted_ephemerals: Vec<[u8; 32]>,
            identity_dh: [u8; 32],
            inbox: Option<[u8; 32]>,
            hello_sent: bool,
        }
        impl crate::Record for V1 {
            const VERSION: u8 = 1;
        }

        let vault = Vault::new([3; 32]);
        let old = V1 {
            ratchet: vec![1, 2, 3],
            pending_init: Vec::new(),
            accepted_ephemerals: vec![[7; 32]],
            identity_dh: [9; 32],
            inbox: Some([4; 32]),
            hello_sent: true,
        };
        let sealed = vault.seal(Table::Peers, b"peer", &old, &mut OsRng);
        let record: PeerRecord = vault.open(Table::Peers, b"peer", &sealed).unwrap();
        assert_eq!(record.alias, None);
        let session = record.legacy.unwrap();
        assert_eq!(session.inbox, None, "that inbox is read no more");
        assert!(session.hello_sent);
        let names = vault.open_contact(b"peer", &sealed).unwrap();
        assert_eq!((names.alias, names.name), (None, None));
    }

    /// Sessions and contacts saved before inboxes turned with the epochs
    /// load without them, keeping the rest.
    #[test]
    fn records_from_before_inbox_epochs_drop_the_inboxes() {
        #[derive(Serialize, Deserialize)]
        struct SessionV1 {
            ratchet: Vec<u8>,
            pending_init: Vec<u8>,
            accepted_ephemerals: Vec<[u8; 32]>,
            inbox: Option<[u8; 32]>,
            hello_sent: bool,
        }
        impl crate::Record for SessionV1 {
            const VERSION: u8 = 1;
        }
        #[derive(Serialize, Deserialize)]
        struct PeerV6 {
            identity_dh: [u8; 32],
            alias: Option<String>,
            name: Option<String>,
            request: bool,
            blocked: bool,
            via: Option<String>,
            devices: Option<Vec<u8>>,
            inboxes: Vec<(u32, [u8; 32])>,
        }
        impl crate::Record for PeerV6 {
            const VERSION: u8 = 6;
        }

        let vault = Vault::new([3; 32]);
        let session = SessionV1 {
            ratchet: vec![1, 2, 3],
            pending_init: Vec::new(),
            accepted_ephemerals: vec![[7; 32]],
            inbox: Some([4; 32]),
            hello_sent: true,
        };
        let sealed = vault.seal(Table::Sessions, b"s", &session, &mut OsRng);
        let record: SessionRecord = vault.open(Table::Sessions, b"s", &sealed).unwrap();
        assert_eq!(record.inbox, None);
        assert_eq!(record.accepted_ephemerals, [[7; 32]]);
        assert!(record.hello_sent);

        let peer = PeerV6 {
            identity_dh: [9; 32],
            alias: Some("Bob".into()),
            name: None,
            request: false,
            blocked: true,
            via: None,
            devices: Some(vec![5]),
            inboxes: vec![(2, [4; 32])],
        };
        let sealed = vault.seal(Table::Peers, b"p", &peer, &mut OsRng);
        let record: PeerRecord = vault.open(Table::Peers, b"p", &sealed).unwrap();
        assert!(record.inboxes.is_empty());
        assert_eq!(record.devices, Some(vec![5]));
        assert!(record.blocked);
    }

    /// Sessions saved before contacts sent their own names keep the alias.
    #[test]
    fn version_2_records_keep_the_alias() {
        #[derive(Serialize, Deserialize)]
        struct V2 {
            ratchet: Vec<u8>,
            pending_init: Vec<u8>,
            accepted_ephemerals: Vec<[u8; 32]>,
            identity_dh: [u8; 32],
            inbox: Option<[u8; 32]>,
            hello_sent: bool,
            alias: Option<String>,
        }
        impl crate::Record for V2 {
            const VERSION: u8 = 2;
        }

        let vault = Vault::new([3; 32]);
        let old = V2 {
            ratchet: vec![1, 2, 3],
            pending_init: Vec::new(),
            accepted_ephemerals: Vec::new(),
            identity_dh: [9; 32],
            inbox: None,
            hello_sent: false,
            alias: Some("Bob".into()),
        };
        let sealed = vault.seal(Table::Peers, b"peer", &old, &mut OsRng);
        let names = vault.open_contact(b"peer", &sealed).unwrap();
        assert_eq!(names.alias.as_deref(), Some("Bob"));
        assert_eq!(names.name, None);
    }

    /// Sessions saved before requests and blocking are plain contacts.
    #[test]
    fn version_3_records_are_contacts() {
        #[derive(Serialize, Deserialize)]
        struct V3 {
            ratchet: Vec<u8>,
            pending_init: Vec<u8>,
            accepted_ephemerals: Vec<[u8; 32]>,
            identity_dh: [u8; 32],
            inbox: Option<[u8; 32]>,
            hello_sent: bool,
            alias: Option<String>,
            name: Option<String>,
        }
        impl crate::Record for V3 {
            const VERSION: u8 = 3;
        }

        let vault = Vault::new([3; 32]);
        let old = V3 {
            ratchet: vec![1, 2, 3],
            pending_init: Vec::new(),
            accepted_ephemerals: Vec::new(),
            identity_dh: [9; 32],
            inbox: None,
            hello_sent: true,
            alias: Some("Bob".into()),
            name: Some("Robert".into()),
        };
        let sealed = vault.seal(Table::Peers, b"peer", &old, &mut OsRng);
        let info = vault.open_contact(b"peer", &sealed).unwrap();
        assert_eq!(info.alias.as_deref(), Some("Bob"));
        assert_eq!(info.name.as_deref(), Some("Robert"));
        assert!(!info.request && !info.blocked);
    }
}
