use cypher_crypto::{InitHeader, Ratchet};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::CoreError;

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
}

/// One Double Ratchet session with one device of a contact.
pub(crate) struct Session {
    pub ratchet: Ratchet,
    /// Initiator side: attached to every message until the responder has
    /// provably received one (any reply decrypts).
    pub pending_init: Option<InitHeader>,
    /// Responder side: ephemeral keys of inits already accepted, so repeated
    /// or replayed copies reuse the live session instead of resetting it.
    pub accepted_ephemerals: Vec<[u8; 32]>,
    /// That device's inbox, as its `Hello` told.
    pub inbox: Option<[u8; 32]>,
    /// Whether our `Hello` went out on this session.
    pub hello_sent: bool,
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
        }
    }

    pub(crate) fn to_record(&self) -> PeerRecord {
        PeerRecord {
            identity_dh: self.identity_dh,
            alias: self.alias.clone(),
            name: self.name.clone(),
            request: self.request,
            blocked: self.blocked,
            via: self.via.clone(),
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
        }
    }
}

impl Session {
    /// A session just made with one device.
    pub(crate) fn new(ratchet: Ratchet, pending_init: Option<InitHeader>) -> Self {
        Self {
            ratchet,
            pending_init,
            accepted_ephemerals: Vec::new(),
            inbox: None,
            hello_sent: false,
        }
    }

    /// Keeps what belongs to the device rather than the old session: its
    /// inbox and the inits seen from it. Our `Hello` goes out again.
    pub(crate) fn carry_over(&mut self, old: Self) {
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

    pub(crate) fn to_record(&self) -> SessionRecord {
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
        }
    }

    pub(crate) fn from_record(r: &SessionRecord) -> Result<Self, CoreError> {
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
            ratchet: Ratchet::from_bytes(&r.ratchet).map_err(|_| CoreError::Storage)?,
            pending_init,
            accepted_ephemerals: r.accepted_ephemerals.clone(),
            inbox: r.inbox,
            hello_sent: r.hello_sent,
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
    /// The one session a record from before devices held, to be moved to
    /// its own row (as the contact's first device) on load.
    #[serde(skip)]
    pub(crate) legacy: Option<SessionRecord>,
}

impl crate::Record for PeerRecord {
    const VERSION: u8 = 5;

    /// Up to version 4 a contact and its one session were stored together:
    /// the session comes out as `legacy`.
    fn upgrade(version: u8, body: &[u8]) -> Result<Self, CoreError> {
        let v4 = PeerRecordV4::upgrade(version, body)?;
        Ok(Self {
            identity_dh: v4.identity_dh,
            alias: v4.alias,
            name: v4.name,
            request: v4.request,
            blocked: v4.blocked,
            via: v4.via,
            legacy: Some(SessionRecord {
                ratchet: v4.ratchet,
                pending_init: v4.pending_init,
                accepted_ephemerals: v4.accepted_ephemerals,
                inbox: v4.inbox,
                hello_sent: v4.hello_sent,
            }),
        })
    }
}

/// One session with one device of a contact, as stored.
#[derive(Serialize, Deserialize)]
pub(crate) struct SessionRecord {
    #[serde(with = "zeroizing_bytes")]
    ratchet: Zeroizing<Vec<u8>>,
    pending_init: Vec<u8>,
    accepted_ephemerals: Vec<[u8; 32]>,
    inbox: Option<[u8; 32]>,
    hello_sent: bool,
}

impl crate::Record for SessionRecord {
    const VERSION: u8 = 1;
}

/// [`PeerRecord`] as version 4 stored it, a contact and its session.
#[derive(Deserialize)]
struct PeerRecordV4 {
    #[serde(with = "zeroizing_bytes")]
    ratchet: Zeroizing<Vec<u8>>,
    pending_init: Vec<u8>,
    accepted_ephemerals: Vec<[u8; 32]>,
    identity_dh: [u8; 32],
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
        assert_eq!(session.inbox, Some([4; 32]));
        assert!(session.hello_sent);
        let names = vault.open_contact(b"peer", &sealed).unwrap();
        assert_eq!((names.alias, names.name), (None, None));
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
