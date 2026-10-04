use std::collections::BTreeMap;

use cypher_crypto::prekey::SignedPreKey;
use cypher_crypto::{IdentityKeyPair, OneTimePreKey, PrekeyBundle};
use cypher_wire::BUNDLE_BASE_LEN;
use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

pub(crate) const OPK_BATCH: u32 = 100;
pub(crate) const OPK_LOW_WATER: u16 = 20;
/// Private one-time prekeys kept for initiators still on their way. The
/// server hands out at most 20 of ours an hour (signaling's per-target
/// limit), so even under a sustained drain this covers days of fetched but
/// not yet used keys; normally it covers years.
const MAX_RETAINED_OPKS: usize = 2000;
const SPK_ROTATION_MS: u64 = 7 * 24 * 3600 * 1000;

pub(crate) struct Prekeys {
    spk: SignedPreKey,
    spk_created_ms: u64,
    prev_spk: Option<SignedPreKey>,
    opks: BTreeMap<u32, OneTimePreKey>,
    next_opk_id: u32,
}

impl Prekeys {
    pub(crate) fn generate(now_ms: u64, rng: &mut impl CryptoRngCore) -> Self {
        Self {
            spk: SignedPreKey::generate(1, rng),
            spk_created_ms: now_ms,
            prev_spk: None,
            opks: BTreeMap::new(),
            next_opk_id: 1,
        }
    }

    /// Signed bundle without a one-time prekey, as published to the server.
    pub(crate) fn base_bundle(&self, identity: &IdentityKeyPair) -> Vec<u8> {
        let mut out = Vec::with_capacity(PrekeyBundle::MAX_LEN);
        PrekeyBundle::new(identity, &self.spk, None).encode(&mut out);
        out.truncate(BUNDLE_BASE_LEN);
        out
    }

    pub(crate) fn new_batch(&mut self, rng: &mut impl CryptoRngCore) -> Vec<(u32, [u8; 32])> {
        let batch: Vec<_> = (0..OPK_BATCH)
            .map(|_| {
                let id = self.next_opk_id;
                self.next_opk_id = self.next_opk_id.wrapping_add(1).max(1);
                let opk = OneTimePreKey::generate(id, rng);
                let public = (id, opk.public());
                self.opks.insert(id, opk);
                public
            })
            .collect();
        while self.opks.len() > MAX_RETAINED_OPKS {
            self.opks.pop_first();
        }
        batch
    }

    pub(crate) fn spk(&self, id: u32) -> Option<&SignedPreKey> {
        if self.spk.id() == id {
            Some(&self.spk)
        } else {
            self.prev_spk.as_ref().filter(|k| k.id() == id)
        }
    }

    pub(crate) fn take_opk(&mut self, id: u32) -> Option<OneTimePreKey> {
        self.opks.remove(&id)
    }

    pub(crate) fn opk(&self, id: u32) -> Option<&OneTimePreKey> {
        self.opks.get(&id)
    }

    pub(crate) fn rotate_if_due(&mut self, now_ms: u64, rng: &mut impl CryptoRngCore) -> bool {
        if now_ms.saturating_sub(self.spk_created_ms) < SPK_ROTATION_MS {
            return false;
        }
        let next = SignedPreKey::generate(self.spk.id().wrapping_add(1).max(1), rng);
        self.prev_spk = Some(std::mem::replace(&mut self.spk, next));
        self.spk_created_ms = now_ms;
        true
    }

    pub(crate) fn to_record(&self) -> PrekeysRecord {
        PrekeysRecord {
            spk: (self.spk.id(), *self.spk.secret_bytes()),
            spk_created_ms: self.spk_created_ms,
            prev_spk: self.prev_spk.as_ref().map(|k| (k.id(), *k.secret_bytes())),
            opks: self
                .opks
                .values()
                .map(|k| (k.id(), *k.secret_bytes()))
                .collect(),
            next_opk_id: self.next_opk_id,
        }
    }

    pub(crate) fn from_record(r: &PrekeysRecord) -> Self {
        Self {
            spk: SignedPreKey::from_secret_bytes(r.spk.0, r.spk.1),
            spk_created_ms: r.spk_created_ms,
            prev_spk: r
                .prev_spk
                .map(|(id, s)| SignedPreKey::from_secret_bytes(id, s)),
            opks: r
                .opks
                .iter()
                .map(|&(id, s)| (id, OneTimePreKey::from_secret_bytes(id, s)))
                .collect(),
            next_opk_id: r.next_opk_id,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PrekeysRecord {
    spk: (u32, [u8; 32]),
    spk_created_ms: u64,
    prev_spk: Option<(u32, [u8; 32])>,
    opks: Vec<(u32, [u8; 32])>,
    next_opk_id: u32,
}

impl crate::Record for PrekeysRecord {
    const VERSION: u8 = 1;
}

impl Drop for PrekeysRecord {
    fn drop(&mut self) {
        self.spk.1.zeroize();
        if let Some((_, s)) = &mut self.prev_spk {
            s.zeroize();
        }
        self.opks.iter_mut().for_each(|(_, s)| s.zeroize());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn base_bundle_decodes_with_opk_flag() {
        let ik = IdentityKeyPair::generate();
        let pk = Prekeys::generate(0, &mut OsRng);
        let mut full = pk.base_bundle(&ik);
        assert_eq!(full.len(), BUNDLE_BASE_LEN);
        full.push(0);
        let bundle = PrekeyBundle::decode(&full).unwrap();
        bundle.verify().unwrap();
        assert_eq!(bundle.identity, ik.peer_id());
    }

    #[test]
    fn rotation_keeps_previous_spk() {
        let mut pk = Prekeys::generate(0, &mut OsRng);
        assert!(!pk.rotate_if_due(1000, &mut OsRng));
        assert!(pk.rotate_if_due(SPK_ROTATION_MS, &mut OsRng));
        assert!(pk.spk(1).is_some() && pk.spk(2).is_some() && pk.spk(3).is_none());
    }

    #[test]
    fn batches_are_bounded_and_record_roundtrips() {
        let mut pk = Prekeys::generate(0, &mut OsRng);
        let batches = MAX_RETAINED_OPKS / OPK_BATCH as usize + 2;
        for _ in 0..batches {
            pk.new_batch(&mut OsRng);
        }
        let last = u32::try_from(batches).unwrap() * OPK_BATCH;
        assert_eq!(pk.opks.len(), MAX_RETAINED_OPKS);
        assert!(pk.opk(1).is_none() && pk.opk(last).is_some());
        let restored = Prekeys::from_record(&pk.to_record());
        assert!(restored.opk(last).is_some());
        assert_eq!(restored.next_opk_id, last + 1);
    }
}
