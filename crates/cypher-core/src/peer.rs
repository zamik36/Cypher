use cypher_crypto::{InitHeader, Ratchet};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::CoreError;

const MAX_REMEMBERED_INITS: usize = 16;

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
}

impl Peer {
    pub fn is_unconfirmed_initiator(&self) -> bool {
        self.pending_init.is_some()
    }

    pub fn remember_ephemeral(list: &mut Vec<[u8; 32]>, ephemeral: [u8; 32]) {
        if list.len() >= MAX_REMEMBERED_INITS {
            list.remove(0);
        }
        list.push(ephemeral);
    }

    pub fn to_record(&self) -> PeerRecord {
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
        }
    }

    pub fn from_record(r: &PeerRecord) -> Result<Self, CoreError> {
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
}

mod zeroizing_bytes {
    use serde::{Deserialize, Deserializer, Serializer};
    use zeroize::Zeroizing;

    pub fn serialize<S: Serializer>(v: &Zeroizing<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(v)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Zeroizing<Vec<u8>>, D::Error> {
        Vec::<u8>::deserialize(d).map(Zeroizing::new)
    }
}
