//! The onion keys signaling hands out: a random one per epoch, shared by
//! every instance through Redis and forgotten there two days after its
//! epoch, so onion traffic recorded meanwhile cannot be opened later even
//! with this server's files. Redis holds them wrapped under a key derived
//! from the long-term signing key, which signs the set clients fetch.

use std::collections::BTreeMap;
use std::sync::RwLock;

use cypher_crypto::aead::{self, NONCE_LEN};
use cypher_crypto::identity::{IdentityKeyPair, IdentitySeed};
use cypher_crypto::onion_keys::{EpochKey, KeySet};
use hkdf::Hkdf;
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::store::Store;

/// How long one onion key is the one to seal to.
pub(crate) const EPOCH_SECS: u64 = 7 * 24 * 3600;
const DAY_SECS: u64 = 24 * 3600;
/// Clients may seal to a key a day before its epoch and a day after, so a
/// clock that is somewhat off, or a set fetched late, still works.
const MARGIN_SECS: u64 = DAY_SECS;
/// A key is kept this long after its epoch ends, then forgotten.
const KEEP_AFTER_SECS: u64 = 2 * DAY_SECS;
const WRAP_INFO: &[u8] = b"cypher/v4/onion-wrap";

pub(crate) fn epoch_at(now_secs: u64) -> u32 {
    u32::try_from(now_secs / EPOCH_SECS).unwrap_or(u32::MAX)
}

fn start_of(epoch: u32) -> u64 {
    u64::from(epoch) * EPOCH_SECS
}

/// The epoch's key as published: its window includes the margins.
fn published(epoch: u32, secret: &StaticSecret) -> EpochKey {
    let start = start_of(epoch);
    EpochKey {
        epoch,
        public: PublicKey::from(secret).to_bytes(),
        not_before: start.saturating_sub(MARGIN_SECS),
        not_after: start + EPOCH_SECS + MARGIN_SECS,
    }
}

pub(crate) struct OnionKeys {
    signer: IdentityKeyPair,
    wrap: Zeroizing<[u8; 32]>,
    /// The epochs' secrets this instance holds: the previous, the current
    /// and the next.
    known: RwLock<BTreeMap<u32, StaticSecret>>,
}

impl OnionKeys {
    /// Keys signed with the long-term key derived from `signing_seed`.
    pub(crate) fn new(signing_seed: &[u8; 32]) -> Self {
        let mut wrap = Zeroizing::new([0u8; 32]);
        let expanded = Hkdf::<Sha256>::new(None, signing_seed).expand(WRAP_INFO, wrap.as_mut());
        debug_assert!(expanded.is_ok(), "32 bytes is a valid HKDF length");
        Self {
            signer: IdentitySeed(*signing_seed).derive_identity(),
            wrap,
            known: RwLock::new(BTreeMap::new()),
        }
    }

    /// The long-term key clients remember this server by.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "handed out once Bootstrap carries it")
    )]
    pub(crate) fn signing_key(&self) -> [u8; 32] {
        *self.signer.peer_id().as_bytes()
    }

    /// Makes sure the current and the next epoch have a key, the same one on
    /// every instance, and forgets what is older than the previous.
    pub(crate) async fn refresh(&self, store: &Store, now_secs: u64) -> redis::RedisResult<()> {
        let current = epoch_at(now_secs);
        let mut fresh = BTreeMap::new();
        for epoch in [
            current.saturating_sub(1),
            current,
            current.saturating_add(1),
        ] {
            let ttl = (start_of(epoch) + EPOCH_SECS + KEEP_AFTER_SECS).saturating_sub(now_secs);
            let create = epoch >= current;
            if let Some(secret) = self.load(store, epoch, ttl, create).await? {
                fresh.insert(epoch, secret);
            }
        }
        if let Ok(mut known) = self.known.write() {
            *known = fresh;
        }
        Ok(())
    }

    /// The epoch's secret from Redis; created there first if `create` and
    /// no instance has yet.
    async fn load(
        &self,
        store: &Store,
        epoch: u32,
        ttl: u64,
        create: bool,
    ) -> redis::RedisResult<Option<StaticSecret>> {
        if let Some(wrapped) = store.onion_key(epoch).await? {
            return Ok(self.unwrap_key(epoch, &wrapped));
        }
        if !create || ttl == 0 {
            return Ok(None);
        }
        let secret = StaticSecret::random_from_rng(OsRng);
        store
            .put_onion_key(epoch, &self.wrap_key(epoch, &secret), ttl)
            .await?;
        // Another instance may have been first: take what is stored.
        let stored = store.onion_key(epoch).await?;
        Ok(stored.and_then(|wrapped| self.unwrap_key(epoch, &wrapped)))
    }

    fn wrap_key(&self, epoch: u32, secret: &StaticSecret) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let sealed = aead::seal(&self.wrap, &nonce, &epoch.to_le_bytes(), secret.as_bytes());
        [nonce.as_slice(), &sealed].concat()
    }

    fn unwrap_key(&self, epoch: u32, wrapped: &[u8]) -> Option<StaticSecret> {
        let (nonce, sealed) = wrapped.split_first_chunk::<NONCE_LEN>()?;
        let plain =
            Zeroizing::new(aead::open(&self.wrap, nonce, &epoch.to_le_bytes(), sealed).ok()?);
        let bytes: [u8; 32] = plain.as_slice().try_into().ok()?;
        Some(StaticSecret::from(bytes))
    }

    /// The secret to open a request sealed to `epoch`'s key, if this
    /// instance still holds it.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used once requests name their key epoch")
    )]
    pub(crate) fn secret(&self, epoch: u32) -> Option<StaticSecret> {
        self.known.read().ok()?.get(&epoch).cloned()
    }

    /// The signed set of keys clients may seal to now or later.
    pub(crate) fn key_set(&self, now_secs: u64) -> Option<Vec<u8>> {
        let keys: Vec<EpochKey> = self
            .known
            .read()
            .ok()?
            .iter()
            .map(|(epoch, secret)| published(*epoch, secret))
            .filter(|k| k.not_after > now_secs)
            .collect();
        KeySet::sign(&self.signer, now_secs, keys)
            .ok()
            .map(|set| set.encode())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epochs_and_windows_overlap_by_a_margin() {
        let now = 1_000 * EPOCH_SECS + 5;
        assert_eq!(epoch_at(now), 1_000);
        let key = published(1_000, &StaticSecret::random_from_rng(OsRng));
        assert_eq!(key.not_before, start_of(1_000) - MARGIN_SECS);
        assert_eq!(key.not_after, start_of(1_001) + MARGIN_SECS);
    }

    #[test]
    fn a_wrapped_key_opens_only_for_its_epoch_and_server() {
        let keys = OnionKeys::new(&[1; 32]);
        let secret = StaticSecret::random_from_rng(OsRng);
        let wrapped = keys.wrap_key(5, &secret);
        let opened = keys.unwrap_key(5, &wrapped).unwrap();
        assert_eq!(opened.as_bytes(), secret.as_bytes());
        assert!(keys.unwrap_key(6, &wrapped).is_none(), "another epoch");
        assert!(OnionKeys::new(&[2; 32]).unwrap_key(5, &wrapped).is_none());
        assert!(keys.unwrap_key(5, &wrapped[..10]).is_none());
    }
}
