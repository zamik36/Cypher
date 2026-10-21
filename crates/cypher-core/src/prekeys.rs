use std::collections::{BTreeMap, BTreeSet};

use cypher_crypto::prekey::SignedPreKey;
use cypher_crypto::{IdentityKeyPair, OneTimePreKey, PrekeyBundle};
use cypher_types::DeviceId;
use cypher_wire::BUNDLE_BASE_LEN;
use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::CoreError;

pub(crate) const OPK_BATCH: u32 = 100;
pub(crate) const OPK_LOW_WATER: u16 = 20;
/// Private one-time prekeys kept for initiators still on their way. The
/// server hands out at most 20 of ours an hour (signaling's per-target
/// limit), so even under a sustained drain this covers days of fetched but
/// not yet used keys; normally it covers years.
const MAX_RETAINED_OPKS: usize = 2000;
const SPK_ROTATION_MS: u64 = 7 * 24 * 3600 * 1000;
/// Inits accepted on one signed prekey, remembered for as long as the
/// prekey is kept (two rotations) so that a copy of one is refused rather
/// than starting a session again. Far more new contacts and session repairs
/// than a device sees in two weeks; once full, further inits on that prekey
/// are refused, not let in by forgetting older ones. 128 KiB at most.
const MAX_INITS_PER_SPK: usize = 4096;

pub(crate) struct Prekeys {
    spk: SignedPreKey,
    spk_created_ms: u64,
    prev_spk: Option<SignedPreKey>,
    opks: BTreeMap<u32, OneTimePreKey>,
    next_opk_id: u32,
    /// Ephemeral keys of the inits accepted on `spk`, and on `prev_spk`.
    spk_inits: BTreeSet<[u8; 32]>,
    prev_spk_inits: BTreeSet<[u8; 32]>,
}

impl Prekeys {
    pub(crate) fn generate(now_ms: u64, rng: &mut impl CryptoRngCore) -> Self {
        Self {
            spk: SignedPreKey::generate(1, rng),
            spk_created_ms: now_ms,
            prev_spk: None,
            opks: BTreeMap::new(),
            next_opk_id: 1,
            spk_inits: BTreeSet::new(),
            prev_spk_inits: BTreeSet::new(),
        }
    }

    /// `device`'s signed bundle without a one-time prekey, as published to
    /// the server.
    pub(crate) fn base_bundle(&self, identity: &IdentityKeyPair, device: DeviceId) -> Vec<u8> {
        let mut out = Vec::with_capacity(PrekeyBundle::MAX_LEN);
        PrekeyBundle::new(identity, device, &self.spk, None).encode(&mut out);
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

    /// The inits accepted on signed prekey `id`, while it is kept.
    fn inits(&self, id: u32) -> Option<&BTreeSet<[u8; 32]>> {
        if self.spk.id() == id {
            Some(&self.spk_inits)
        } else {
            self.prev_spk
                .as_ref()
                .filter(|k| k.id() == id)
                .map(|_| &self.prev_spk_inits)
        }
    }

    /// Whether an init with this ephemeral key was accepted on signed
    /// prekey `spk_id`.
    pub(crate) fn accepted_init(&self, spk_id: u32, ephemeral: &[u8; 32]) -> bool {
        self.inits(spk_id).is_some_and(|i| i.contains(ephemeral))
    }

    /// Whether another init on signed prekey `spk_id` can be remembered.
    pub(crate) fn has_room_for_init(&self, spk_id: u32) -> bool {
        self.inits(spk_id)
            .is_some_and(|i| i.len() < MAX_INITS_PER_SPK)
    }

    /// Remembers an init accepted on signed prekey `spk_id`; one on a
    /// prekey no longer kept, or beyond the limit, is not.
    pub(crate) fn remember_init(&mut self, spk_id: u32, ephemeral: [u8; 32]) {
        if !self.has_room_for_init(spk_id) {
            return;
        }
        if self.spk.id() == spk_id {
            self.spk_inits.insert(ephemeral);
        } else {
            self.prev_spk_inits.insert(ephemeral);
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
        self.prev_spk_inits = std::mem::take(&mut self.spk_inits);
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
            spk_inits: self.spk_inits.iter().copied().collect(),
            prev_spk_inits: self.prev_spk_inits.iter().copied().collect(),
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
            spk_inits: r.spk_inits.iter().copied().collect(),
            prev_spk_inits: r.prev_spk_inits.iter().copied().collect(),
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
    spk_inits: Vec<[u8; 32]>,
    prev_spk_inits: Vec<[u8; 32]>,
}

impl crate::Record for PrekeysRecord {
    const VERSION: u8 = 2;

    /// Version 1 did not remember the inits accepted: none are known, and
    /// those accepted before the update are covered by their sessions only.
    fn upgrade(version: u8, body: &[u8]) -> Result<Self, CoreError> {
        if version != 1 {
            return Err(CoreError::Storage);
        }
        let mut v1: PrekeysRecordV1 = postcard::from_bytes(body).map_err(|_| CoreError::Storage)?;
        Ok(Self {
            spk: v1.spk,
            spk_created_ms: v1.spk_created_ms,
            prev_spk: v1.prev_spk,
            opks: std::mem::take(&mut v1.opks),
            next_opk_id: v1.next_opk_id,
            spk_inits: Vec::new(),
            prev_spk_inits: Vec::new(),
        })
    }
}

/// [`PrekeysRecord`] as version 1 stored it.
#[derive(Serialize, Deserialize)]
struct PrekeysRecordV1 {
    spk: (u32, [u8; 32]),
    spk_created_ms: u64,
    prev_spk: Option<(u32, [u8; 32])>,
    opks: Vec<(u32, [u8; 32])>,
    next_opk_id: u32,
}

impl Drop for PrekeysRecordV1 {
    fn drop(&mut self) {
        self.spk.1.zeroize();
        if let Some((_, s)) = &mut self.prev_spk {
            s.zeroize();
        }
        self.opks.iter_mut().for_each(|(_, s)| s.zeroize());
    }
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
        let mut full = pk.base_bundle(&ik, DeviceId(2));
        assert_eq!(full.len(), BUNDLE_BASE_LEN);
        full.push(0);
        let bundle = PrekeyBundle::decode(&full).unwrap();
        bundle.verify().unwrap();
        assert_eq!(
            (bundle.identity, bundle.device),
            (ik.peer_id(), DeviceId(2))
        );
    }

    #[test]
    fn rotation_keeps_previous_spk() {
        let mut pk = Prekeys::generate(0, &mut OsRng);
        assert!(!pk.rotate_if_due(1000, &mut OsRng));
        assert!(pk.rotate_if_due(SPK_ROTATION_MS, &mut OsRng));
        assert!(pk.spk(1).is_some() && pk.spk(2).is_some() && pk.spk(3).is_none());
    }

    #[test]
    fn accepted_inits_follow_their_prekey() {
        let mut pk = Prekeys::generate(0, &mut OsRng);
        pk.remember_init(1, [7; 32]);
        pk.remember_init(9, [8; 32]);
        assert!(pk.accepted_init(1, &[7; 32]));
        assert!(!pk.accepted_init(1, &[8; 32]) && !pk.accepted_init(9, &[8; 32]));

        assert!(pk.rotate_if_due(SPK_ROTATION_MS, &mut OsRng));
        assert!(
            pk.accepted_init(1, &[7; 32]),
            "kept with the previous prekey"
        );
        let restored = Prekeys::from_record(&pk.to_record());
        assert!(restored.accepted_init(1, &[7; 32]), "saved");

        assert!(pk.rotate_if_due(2 * SPK_ROTATION_MS, &mut OsRng));
        assert!(!pk.accepted_init(1, &[7; 32]), "gone with its prekey");
    }

    #[test]
    fn a_full_prekey_takes_no_more_inits() {
        let mut pk = Prekeys::generate(0, &mut OsRng);
        for n in 0..MAX_INITS_PER_SPK {
            assert!(pk.has_room_for_init(1));
            let mut e = [0; 32];
            e[..8].copy_from_slice(&n.to_le_bytes());
            pk.remember_init(1, e);
        }
        assert!(!pk.has_room_for_init(1));
        pk.remember_init(1, [0xFF; 32]);
        assert!(!pk.accepted_init(1, &[0xFF; 32]));
        assert!(pk.accepted_init(1, &[0; 32]), "none forgotten");
    }

    /// A record written by version 1 reads back with its keys and no
    /// accepted inits.
    #[test]
    fn version_1_records_are_upgraded() {
        let mut pk = Prekeys::generate(0, &mut OsRng);
        pk.new_batch(&mut OsRng);
        let r = pk.to_record();
        let v1 = PrekeysRecordV1 {
            spk: r.spk,
            spk_created_ms: r.spk_created_ms,
            prev_spk: r.prev_spk,
            opks: r.opks.clone(),
            next_opk_id: r.next_opk_id,
        };
        let body = postcard::to_allocvec(&v1).unwrap();
        let upgraded = <PrekeysRecord as crate::Record>::upgrade(1, &body).unwrap();
        let restored = Prekeys::from_record(&upgraded);
        assert_eq!(restored.spk.secret_bytes(), pk.spk.secret_bytes());
        assert_eq!(restored.opks.len(), pk.opks.len());
        assert_eq!(restored.next_opk_id, pk.next_opk_id);
        assert!(restored.spk_inits.is_empty() && restored.prev_spk_inits.is_empty());
        assert!(<PrekeysRecord as crate::Record>::upgrade(0, &body).is_err());
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "generates 2200 X25519 keys: over 20 minutes under Miri"
    )]
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
