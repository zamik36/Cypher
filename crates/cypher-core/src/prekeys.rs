use std::collections::{BTreeMap, BTreeSet};

use cypher_crypto::prekey::SignedPreKey;
use cypher_crypto::{IdentityKeyPair, OneTimePreKey, PrekeyBundle};
use cypher_types::{DeviceId, PeerId};
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
/// prekey is kept so that a copy of one is refused rather than starting a
/// session again. A device accepts a few a week (new contacts, session
/// repairs, its own new devices); reaching this many means someone is
/// flooding it, and the prekey is rotated early rather than forgetting any:
/// the full set goes with the previous prekey, and initiators that fetched
/// the one before that retry with fresh keys. Every accepted init rewrites
/// the prekeys record, so this also bounds it: two sets of 32 KiB.
const MAX_INITS_PER_SPK: usize = 1024;
/// Inits taken from the sessions when prekeys saved before inits were
/// remembered are read: half a set, see [`Prekeys::seed_inits`].
const MAX_SEEDED_INITS: usize = MAX_INITS_PER_SPK / 2;
/// Inits accepted on one signed prekey from one sender still waiting to be
/// taken as a contact. Each starts a session afresh; an honest stranger
/// needs one, a few more when sessions fail. Beyond it, only that sender is
/// refused. Counts are kept in memory: a restart grants another round,
/// which a sender cannot cause.
const MAX_REQUEST_INITS: u16 = 32;
/// Inits accepted on one signed prekey from one contact (all its devices,
/// or our own other devices). A device repairs a session with another at
/// most every ten minutes, so a week of honest use stays far below; with
/// this, no single contact can fill a prekey and keep rotating it, as that
/// takes [`MAX_INITS_PER_SPK`] / this many senders.
const MAX_CONTACT_INITS: u16 = 256;

/// Who sent an init, as far as how many of theirs are taken goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InitSender {
    /// Someone not (yet) taken as a contact.
    Request(PeerId),
    /// A contact, or this identity's own other device.
    Contact(PeerId),
}

impl InitSender {
    fn peer(self) -> PeerId {
        match self {
            Self::Request(peer) | Self::Contact(peer) => peer,
        }
    }

    fn quota(self) -> u16 {
        match self {
            Self::Request(_) => MAX_REQUEST_INITS,
            Self::Contact(_) => MAX_CONTACT_INITS,
        }
    }
}

pub(crate) struct Prekeys {
    spk: SignedPreKey,
    spk_created_ms: u64,
    prev_spk: Option<SignedPreKey>,
    opks: BTreeMap<u32, OneTimePreKey>,
    next_opk_id: u32,
    /// Ephemeral keys of the inits accepted on `spk`, and on `prev_spk`.
    spk_inits: BTreeSet<[u8; 32]>,
    prev_spk_inits: BTreeSet<[u8; 32]>,
    /// Inits accepted per signed prekey from each sender. In memory only.
    sender_inits: BTreeMap<(u32, PeerId), u16>,
    /// Refusals already told, per signed prekey: of a sender beyond its
    /// quota, or (`None`) of a prekey full. In memory only.
    refusals: BTreeSet<(u32, Option<PeerId>)>,
    /// Read from a record that did not remember inits yet.
    unseeded: bool,
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
            sender_inits: BTreeMap::new(),
            refusals: BTreeSet::new(),
            unseeded: false,
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

    /// Whether a new init on signed prekey `spk_id` from `sender` may be
    /// taken. The current prekey always has room (it rotates when full);
    /// the previous one takes no more once full, nor does a sender beyond
    /// its quota.
    pub(crate) fn may_init(&self, spk_id: u32, sender: InitSender) -> bool {
        let room = self
            .inits(spk_id)
            .is_some_and(|i| i.len() < MAX_INITS_PER_SPK);
        let quota = self
            .sender_inits
            .get(&(spk_id, sender.peer()))
            .is_none_or(|n| *n < sender.quota());
        room && quota
    }

    /// Notes that an init on signed prekey `spk_id` from `sender` was
    /// refused; whether it is the first refusal of its kind (this sender
    /// beyond its quota, or this prekey full), the one to tell.
    pub(crate) fn first_refusal(&mut self, spk_id: u32, sender: InitSender) -> bool {
        let full = self
            .inits(spk_id)
            .is_none_or(|i| i.len() >= MAX_INITS_PER_SPK);
        self.refusals
            .insert((spk_id, (!full).then(|| sender.peer())))
    }

    /// Remembers an init accepted on signed prekey `spk_id` from `sender`;
    /// one on a prekey no longer kept, or one that is full, is not. Whether
    /// the current prekey filled up and was rotated, to be published.
    pub(crate) fn remember_init(
        &mut self,
        spk_id: u32,
        ephemeral: [u8; 32],
        sender: InitSender,
        now_ms: u64,
        rng: &mut impl CryptoRngCore,
    ) -> bool {
        let current = self.spk.id() == spk_id;
        let set = if current {
            &mut self.spk_inits
        } else if self.prev_spk.as_ref().is_some_and(|k| k.id() == spk_id) {
            &mut self.prev_spk_inits
        } else {
            return false;
        };
        if set.len() >= MAX_INITS_PER_SPK {
            return false;
        }
        set.insert(ephemeral);
        let n = self
            .sender_inits
            .entry((spk_id, sender.peer()))
            .or_insert(0);
        *n = n.saturating_add(1);
        let full = current && self.spk_inits.len() >= MAX_INITS_PER_SPK;
        if full {
            self.rotate(now_ms, rng);
        }
        full
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
        self.rotate(now_ms, rng);
        true
    }

    /// A fresh signed prekey; the current one and its inits become the
    /// previous, and the previous ones go.
    fn rotate(&mut self, now_ms: u64, rng: &mut impl CryptoRngCore) {
        let next = SignedPreKey::generate(self.spk.id().wrapping_add(1).max(1), rng);
        let kept = self.spk.id();
        self.prev_spk = Some(std::mem::replace(&mut self.spk, next));
        self.prev_spk_inits = std::mem::take(&mut self.spk_inits);
        self.sender_inits.retain(|(id, _), _| *id == kept);
        self.refusals.retain(|(id, _)| *id == kept);
        self.spk_created_ms = now_ms;
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
            from_v1: false,
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
            sender_inits: BTreeMap::new(),
            refusals: BTreeSet::new(),
            unseeded: r.from_v1,
        }
    }

    /// Whether these prekeys come from a record that did not remember the
    /// inits accepted; asked once.
    pub(crate) fn take_unseeded(&mut self) -> bool {
        std::mem::take(&mut self.unseeded)
    }

    /// Takes inits accepted before they were remembered with the prekeys
    /// (on which prekey is not known) as accepted on both kept, filling
    /// each to half at most. This is a one-off and best effort; seeding
    /// leaves half the current set for new inits, so rotation still
    /// happens only when an init is accepted, never at startup, where it
    /// would drop a prekey initiators may still hold.
    pub(crate) fn seed_inits(&mut self, ephemerals: &[[u8; 32]]) {
        for set in [&mut self.spk_inits, &mut self.prev_spk_inits] {
            for e in ephemerals {
                if set.len() >= MAX_SEEDED_INITS {
                    break;
                }
                set.insert(*e);
            }
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
    /// Upgraded from version 1, which did not remember inits. Not stored.
    #[serde(skip)]
    from_v1: bool,
}

impl crate::Record for PrekeysRecord {
    const VERSION: u8 = 2;

    /// Version 1 did not remember the inits accepted: the core takes those
    /// its sessions remember instead.
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
            from_v1: true,
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

    /// A contact.
    const CONTACT: InitSender = InitSender::Contact(PeerId([9; 32]));

    /// Remembers an init from a contact on `spk_id`; whether that rotated.
    fn remember(pk: &mut Prekeys, spk_id: u32, ephemeral: [u8; 32]) -> bool {
        pk.remember_init(spk_id, ephemeral, CONTACT, 0, &mut OsRng)
    }

    #[test]
    fn accepted_inits_follow_their_prekey() {
        let mut pk = Prekeys::generate(0, &mut OsRng);
        remember(&mut pk, 1, [7; 32]);
        remember(&mut pk, 9, [8; 32]);
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

    /// An ephemeral key told apart by `n`.
    fn ephemeral(n: usize) -> [u8; 32] {
        let mut e = [0; 32];
        e[..8].copy_from_slice(&n.to_le_bytes());
        e
    }

    /// A prekey that fills up is rotated, keeping every init it took; the
    /// previous one, full, takes no more, and the one before it goes.
    #[test]
    fn a_full_prekey_rotates_early_and_forgets_none() {
        let mut pk = Prekeys::generate(0, &mut OsRng);
        for n in 0..MAX_INITS_PER_SPK - 1 {
            assert!(!remember(&mut pk, 1, ephemeral(n)));
        }
        assert!(pk.may_init(1, InitSender::Contact(PeerId([3; 32]))));
        assert!(
            remember(&mut pk, 1, ephemeral(MAX_INITS_PER_SPK)),
            "rotated"
        );
        assert!(pk.spk(2).is_some(), "a fresh prekey");
        assert!(pk.accepted_init(1, &ephemeral(0)), "none forgotten");
        let other = InitSender::Contact(PeerId([3; 32]));
        assert!(!pk.may_init(1, other), "the full one takes no more");
        assert!(pk.may_init(2, other));
        assert!(!remember(&mut pk, 1, [0xFF; 32]));
        assert!(!pk.accepted_init(1, &[0xFF; 32]));

        for n in 0..MAX_INITS_PER_SPK {
            remember(&mut pk, 2, ephemeral(n));
        }
        assert!(pk.spk(1).is_none() && pk.spk(3).is_some());
        assert!(pk.accepted_init(2, &ephemeral(0)));
    }

    /// One request may have only so many inits accepted per prekey; others,
    /// and contacts, are not held back by it.
    #[test]
    fn a_request_has_a_quota_of_inits() {
        let mut pk = Prekeys::generate(0, &mut OsRng);
        let stranger = InitSender::Request(PeerId([1; 32]));
        let other = InitSender::Request(PeerId([2; 32]));
        for n in 0..usize::from(MAX_REQUEST_INITS) {
            assert!(pk.may_init(1, stranger));
            pk.remember_init(1, ephemeral(n), stranger, 0, &mut OsRng);
        }
        assert!(!pk.may_init(1, stranger));
        assert!(pk.may_init(1, other) && pk.may_init(1, CONTACT));
        assert!(pk.rotate_if_due(SPK_ROTATION_MS, &mut OsRng));
        assert!(pk.may_init(2, stranger), "a fresh quota on a fresh prekey");
        assert!(!pk.may_init(1, stranger), "kept on the previous one");
    }

    /// A contact, too, may have only so many inits accepted per prekey, so
    /// that it alone cannot fill one; other contacts are not held back.
    #[test]
    fn a_contact_has_a_quota_of_inits() {
        let mut pk = Prekeys::generate(0, &mut OsRng);
        let other = InitSender::Contact(PeerId([2; 32]));
        for n in 0..usize::from(MAX_CONTACT_INITS) {
            assert!(pk.may_init(1, CONTACT));
            pk.remember_init(1, ephemeral(n), CONTACT, 0, &mut OsRng);
        }
        assert!(!pk.may_init(1, CONTACT));
        assert!(pk.may_init(1, other));
        assert!(pk.spk(2).is_none(), "far from rotating");
        assert!(pk.first_refusal(1, CONTACT) && !pk.first_refusal(1, CONTACT));
    }

    /// However many inits the sessions remember, seeding leaves room: a
    /// contact's next init is taken.
    #[test]
    fn seeding_leaves_room_for_new_inits() {
        let mut pk = Prekeys::generate(0, &mut OsRng);
        let seeds: Vec<_> = (0..MAX_INITS_PER_SPK).map(ephemeral).collect();
        pk.seed_inits(&seeds);
        assert!(pk.accepted_init(1, &seeds[0]));
        assert!(pk.may_init(1, CONTACT));
        assert!(!remember(&mut pk, 1, [0xEE; 32]), "no rotation");
        assert!(pk.accepted_init(1, &[0xEE; 32]));
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
        let mut restored = Prekeys::from_record(&upgraded);
        assert!(restored.take_unseeded() && !restored.take_unseeded());
        assert!(!Prekeys::from_record(&pk.to_record()).take_unseeded());
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
