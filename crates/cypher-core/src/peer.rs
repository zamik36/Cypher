use cypher_crypto::{InitHeader, Ratchet};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::CoreError;

const MAX_REMEMBERED_INITS: usize = 16;
/// Longest name the user may give a contact, in characters.
pub(crate) const MAX_ALIAS_CHARS: usize = 64;

pub(crate) struct Peer {
    pub ratchet: Ratchet,
    /// Initiator side: attached to every message until the responder has
    /// provably received one (any reply decrypts).
    pub pending_init: Option<InitHeader>,
    /// Responder side: ephemeral keys of inits already accepted, so repeated
    /// or replayed copies reuse the live session instead of resetting it.
    pub accepted_ephemerals: Vec<[u8; 32]>,
    pub identity_dh: [u8; 32],
    pub inbox: Option<[u8; 32]>,
    pub hello_sent: bool,
    /// The name the user gave this contact; only ever stored on this device.
    pub alias: Option<String>,
}

/// A contact name as the user typed it, made safe to show: control
/// characters dropped, trimmed, at most [`MAX_ALIAS_CHARS`]; empty means none.
pub(crate) fn clean_alias(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_ALIAS_CHARS)
        .collect();
    let cleaned = cleaned.trim_end();
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

impl Peer {
    pub(crate) fn is_unconfirmed_initiator(&self) -> bool {
        self.pending_init.is_some()
    }

    pub(crate) fn remember_ephemeral(list: &mut Vec<[u8; 32]>, ephemeral: [u8; 32]) {
        if list.len() >= MAX_REMEMBERED_INITS {
            list.remove(0);
        }
        list.push(ephemeral);
    }

    pub(crate) fn to_record(&self) -> PeerRecord {
        let mut init = Vec::new();
        if let Some(h) = &self.pending_init {
            h.encode(&mut init);
        }
        PeerRecord {
            ratchet: self.ratchet.to_bytes(),
            pending_init: init,
            accepted_ephemerals: self.accepted_ephemerals.clone(),
            identity_dh: self.identity_dh,
            inbox: self.inbox,
            hello_sent: self.hello_sent,
            alias: self.alias.clone(),
        }
    }

    pub(crate) fn from_record(r: &PeerRecord) -> Result<Self, CoreError> {
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
            identity_dh: r.identity_dh,
            inbox: r.inbox,
            hello_sent: r.hello_sent,
            alias: r.alias.clone(),
        })
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PeerRecord {
    #[serde(with = "zeroizing_bytes")]
    ratchet: Zeroizing<Vec<u8>>,
    pending_init: Vec<u8>,
    accepted_ephemerals: Vec<[u8; 32]>,
    identity_dh: [u8; 32],
    inbox: Option<[u8; 32]>,
    hello_sent: bool,
    pub(crate) alias: Option<String>,
}

impl crate::Record for PeerRecord {
    const VERSION: u8 = 2;

    /// Version 1 had no contact name.
    fn upgrade(version: u8, body: &[u8]) -> Result<Self, CoreError> {
        if version != 1 {
            return Err(CoreError::Storage);
        }
        let v1: PeerRecordV1 = postcard::from_bytes(body).map_err(|_| CoreError::Storage)?;
        Ok(Self {
            ratchet: v1.ratchet,
            pending_init: v1.pending_init,
            accepted_ephemerals: v1.accepted_ephemerals,
            identity_dh: v1.identity_dh,
            inbox: v1.inbox,
            hello_sent: v1.hello_sent,
            alias: None,
        })
    }
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
        assert_eq!(clean_alias("  Anna  "), Some("Anna".into()));
        assert_eq!(clean_alias("An\u{0}na\n"), Some("Anna".into()));
        assert_eq!(clean_alias(" \t "), None);
        let long = "я".repeat(MAX_ALIAS_CHARS + 10);
        assert_eq!(
            clean_alias(&long).map(|a| a.chars().count()),
            Some(MAX_ALIAS_CHARS)
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
        assert_eq!(record.inbox, Some([4; 32]));
        assert!(record.hello_sent);
        assert_eq!(vault.open_contact_alias(b"peer", &sealed).unwrap(), None);
    }
}
