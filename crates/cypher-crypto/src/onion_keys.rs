//! The onion keys a server publishes, signed with its long-term key.
//!
//! Signaling seals nothing with them; clients seal their anonymous requests
//! to them. Each key is meant for one epoch and the server forgets it some
//! time after: what was recorded on the way cannot be opened later. Clients
//! remember the signing key of a server the first time they meet it and
//! take only key sets it signed, so a server cannot hand one user keys of
//! their own to recognise their requests by.

use cypher_types::PeerId;

use crate::error::CryptoError;
use crate::identity::{IdentityKeyPair, verify_signature};
use crate::reader::Reader;

const KEYSET_SIGNATURE_CONTEXT: &[u8] = b"cypher/v4/onion-keyset";
const EPOCH_KEY_LEN: usize = 4 + 32 + 8 + 8;

/// One onion key and when clients may seal to it, in unix seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EpochKey {
    pub epoch: u32,
    pub public: [u8; 32],
    pub not_before: u64,
    pub not_after: u64,
}

impl EpochKey {
    fn valid_at(&self, now_secs: u64) -> bool {
        (self.not_before..self.not_after).contains(&now_secs)
    }
}

/// Onion keys signed by a server's long-term key: at least one, at most
/// [`KeySet::MAX_KEYS`], in increasing epochs, each with a window that is
/// not empty. Only built by signing or by decoding a correctly signed set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeySet {
    signing_key: [u8; 32],
    issued_at: u64,
    keys: Vec<EpochKey>,
    signature: [u8; 64],
}

impl KeySet {
    pub const MAX_KEYS: usize = 4;
    /// Longest encoding: signing key, issue time, count, keys, signature.
    pub const MAX_LEN: usize = 32 + 8 + 1 + EPOCH_KEY_LEN * Self::MAX_KEYS + 64;

    /// Signs `keys`, issued at `issued_at`, with the server's key.
    pub fn sign(
        signer: &IdentityKeyPair,
        issued_at: u64,
        keys: Vec<EpochKey>,
    ) -> Result<Self, CryptoError> {
        let mut set = Self {
            signing_key: *signer.peer_id().as_bytes(),
            issued_at,
            keys,
            signature: [0; 64],
        };
        set.check()?;
        set.signature = signer.sign(&set.signed_message()).to_bytes();
        Ok(set)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.signed_body();
        out.extend_from_slice(&self.signature);
        out
    }

    /// Decodes a key set and checks that the server key remembered for it,
    /// `pinned`, signed it.
    pub fn verify(bytes: &[u8], pinned: &[u8; 32]) -> Result<Self, CryptoError> {
        let mut r = Reader::new(bytes);
        let signing_key = r.array()?;
        let issued_at = r.u64()?;
        let count = usize::from(r.u8()?);
        if count > Self::MAX_KEYS {
            return Err(CryptoError::Malformed);
        }
        let keys = (0..count)
            .map(|_| {
                Ok(EpochKey {
                    epoch: r.u32()?,
                    public: r.array()?,
                    not_before: r.u64()?,
                    not_after: r.u64()?,
                })
            })
            .collect::<Result<Vec<_>, CryptoError>>()?;
        let signature = r.array()?;
        r.finish()?;
        let set = Self {
            signing_key,
            issued_at,
            keys,
            signature,
        };
        if set.signing_key != *pinned {
            return Err(CryptoError::Signature);
        }
        set.check()?;
        verify_signature(
            &PeerId(set.signing_key),
            &set.signed_message(),
            &set.signature,
        )?;
        Ok(set)
    }

    pub fn signing_key(&self) -> &[u8; 32] {
        &self.signing_key
    }

    pub fn issued_at(&self) -> u64 {
        self.issued_at
    }

    pub fn keys(&self) -> &[EpochKey] {
        &self.keys
    }

    /// The key to seal to at `now_secs`: of those valid then, the newest.
    pub fn pick(&self, now_secs: u64) -> Option<&EpochKey> {
        self.keys.iter().rev().find(|k| k.valid_at(now_secs))
    }

    /// Until when this set has a key to offer: then a newer one is needed.
    pub fn valid_until(&self) -> u64 {
        self.keys.iter().map(|k| k.not_after).max().unwrap_or(0)
    }

    fn check(&self) -> Result<(), CryptoError> {
        let sized = (1..=Self::MAX_KEYS).contains(&self.keys.len());
        let ordered = self.keys.is_sorted_by(|a, b| a.epoch < b.epoch);
        let windows = self.keys.iter().all(|k| k.not_before < k.not_after);
        if sized && ordered && windows {
            Ok(())
        } else {
            Err(CryptoError::Malformed)
        }
    }

    fn signed_body(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::MAX_LEN);
        out.extend_from_slice(&self.signing_key);
        out.extend_from_slice(&self.issued_at.to_le_bytes());
        // At most MAX_KEYS, checked on every way in.
        out.push(u8::try_from(self.keys.len()).unwrap_or(u8::MAX));
        for key in &self.keys {
            out.extend_from_slice(&key.epoch.to_le_bytes());
            out.extend_from_slice(&key.public);
            out.extend_from_slice(&key.not_before.to_le_bytes());
            out.extend_from_slice(&key.not_after.to_le_bytes());
        }
        out
    }

    fn signed_message(&self) -> Vec<u8> {
        [KEYSET_SIGNATURE_CONTEXT, &self.signed_body()].concat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(epoch: u32, not_before: u64, not_after: u64) -> EpochKey {
        EpochKey {
            epoch,
            public: [u8::try_from(epoch).unwrap(); 32],
            not_before,
            not_after,
        }
    }

    fn keys() -> Vec<EpochKey> {
        vec![key(7, 100, 300), key(8, 250, 500)]
    }

    #[test]
    fn a_set_round_trips_and_offers_the_newest_valid_key() {
        let server = IdentityKeyPair::generate();
        let set = KeySet::sign(&server, 200, keys()).unwrap();
        let pinned = *server.peer_id().as_bytes();
        let back = KeySet::verify(&set.encode(), &pinned).unwrap();
        assert_eq!(back, set);
        assert!(set.encode().len() <= KeySet::MAX_LEN);
        assert_eq!(back.pick(99), None);
        assert_eq!(back.pick(200).map(|k| k.epoch), Some(7));
        assert_eq!(
            back.pick(260).map(|k| k.epoch),
            Some(8),
            "the newer once both are"
        );
        assert_eq!(back.pick(500), None);
        assert_eq!(back.valid_until(), 500);
        assert_eq!(back.issued_at(), 200);
    }

    #[test]
    fn a_set_signed_by_another_key_or_altered_is_refused() {
        let server = IdentityKeyPair::generate();
        let pinned = *server.peer_id().as_bytes();
        let other = IdentityKeyPair::generate();
        let forged = KeySet::sign(&other, 200, keys()).unwrap().encode();
        // Not signed by the key remembered for this server.
        KeySet::verify(&forged, &pinned).unwrap_err();

        let good = KeySet::sign(&server, 200, keys()).unwrap().encode();
        for at in [33, 41 + 4, good.len() - 1] {
            let mut bad = good.clone();
            bad[at] ^= 1;
            KeySet::verify(&bad, &pinned).unwrap_err();
        }
        KeySet::verify(&good[..good.len() - 1], &pinned).unwrap_err();
        KeySet::verify(&[good, vec![0]].concat(), &pinned).unwrap_err();
    }

    #[test]
    fn malformed_sets_cannot_be_signed() {
        let server = IdentityKeyPair::generate();
        KeySet::sign(&server, 0, Vec::new()).unwrap_err();
        // A window with no time in it.
        KeySet::sign(&server, 0, vec![key(1, 5, 5)]).unwrap_err();
        KeySet::sign(&server, 0, vec![key(2, 0, 9), key(1, 0, 9)]).unwrap_err();
        let five = (1..=5).map(|e| key(e, 0, 9)).collect();
        KeySet::sign(&server, 0, five).unwrap_err();
    }
}
