mod skipped;

use std::collections::{HashSet, VecDeque};

use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, Zeroizing};

use crate::aead;
use crate::error::CryptoError;
use crate::kdf::{kdf_ck, kdf_rk, message_keys};

pub use skipped::{Changes, MAX_SKIPPED_AGE_MS, MAX_STORED_SKIPPED, SkippedKey};
use skipped::{SkipKey, SkippedKeys};

pub const HEADER_LEN: usize = 40;

/// Most message keys a single incoming message may force us to derive.
pub const MAX_SKIP: u32 = 1000;

/// Messages a session remembers having decrypted, so that a copy of one
/// coming again (an inbox item handed out again after its ack was lost, a
/// frame replayed) is known as such rather than failing as if the session
/// had drifted apart. The core holds it to several inbox batches; 8 KiB of
/// digests per session.
pub const MAX_DECRYPTED: usize = 256;

/// Version 3 keeps the skipped keys out of the snapshot: they are stored
/// one by one ([`SkippedKey`]). Version 2 held them inside.
const SNAPSHOT_VERSION: u8 = 3;
const SNAPSHOT_V2: u8 = 2;
const AAD_CONTEXT: &[u8] = b"cypher/v2";
/// Context, both identity keys and the ratchet header.
const AAD_FIXED_LEN: usize = AAD_CONTEXT.len() + 64 + HEADER_LEN;

/// Per-message ratchet header. Sent in clear, authenticated as AAD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub dh: [u8; 32],
    pub pn: u32,
    pub n: u32,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[..32].copy_from_slice(&self.dh);
        out[32..36].copy_from_slice(&self.pn.to_le_bytes());
        out[36..].copy_from_slice(&self.n.to_le_bytes());
        out
    }

    pub fn decode(b: &[u8]) -> Result<Self, CryptoError> {
        let mut r = crate::reader::Reader::new(b);
        let header = Self {
            dh: r.array()?,
            pn: r.u32()?,
            n: r.u32()?,
        };
        r.finish()?;
        Ok(header)
    }
}

/// Digests of the last [`MAX_DECRYPTED`] messages decrypted.
#[derive(Default)]
struct Decrypted {
    set: HashSet<[u8; 32]>,
    /// Oldest first.
    order: VecDeque<[u8; 32]>,
}

impl Decrypted {
    fn contains(&self, digest: &[u8; 32]) -> bool {
        self.set.contains(digest)
    }

    fn insert(&mut self, digest: [u8; 32]) {
        if !self.set.insert(digest) {
            return;
        }
        self.order.push_back(digest);
        while self.order.len() > MAX_DECRYPTED {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
    }
}

/// Double Ratchet session state (Signal spec, HMAC chain KDF, AES-256-GCM).
pub struct Ratchet {
    ad: [[u8; 32]; 2],
    rk: [u8; 32],
    dhs: StaticSecret,
    dhs_pub: [u8; 32],
    dhr: Option<[u8; 32]>,
    cks: Option<[u8; 32]>,
    ckr: Option<[u8; 32]>,
    ns: u32,
    nr: u32,
    pn: u32,
    /// Kept apart from the snapshot, see [`Ratchet::take_changes`].
    skipped: SkippedKeys,
    /// The last [`MAX_DECRYPTED`] messages decrypted. In memory only: not
    /// part of the snapshot.
    decrypted: Decrypted,
}

struct Step {
    rk: Zeroizing<[u8; 32]>,
    dhs: StaticSecret,
    cks: Zeroizing<[u8; 32]>,
}

/// Everything `decrypt` would change, computed without touching `self` so a
/// forged message can never desynchronise the session.
struct Plan {
    step: Option<Step>,
    skipped: Vec<(SkipKey, Zeroizing<[u8; 32]>)>,
    ckr: Zeroizing<[u8; 32]>,
    nr: u32,
    mk: Zeroizing<[u8; 32]>,
}

impl Ratchet {
    pub(crate) fn init_initiator(
        sk: &[u8; 32],
        ad: [[u8; 32]; 2],
        their_spk: &[u8; 32],
        rng: &mut impl CryptoRngCore,
    ) -> Result<Self, CryptoError> {
        let dhs = StaticSecret::random_from_rng(rng);
        let (rk, cks) = kdf_rk(sk, &*dh(&dhs, their_spk)?);
        Ok(Self {
            ad,
            rk,
            dhs_pub: PublicKey::from(&dhs).to_bytes(),
            dhs,
            dhr: Some(*their_spk),
            cks: Some(cks),
            ckr: None,
            ns: 0,
            nr: 0,
            pn: 0,
            skipped: SkippedKeys::default(),
            decrypted: Decrypted::default(),
        })
    }

    pub(crate) fn init_responder(sk: &[u8; 32], ad: [[u8; 32]; 2], spk: StaticSecret) -> Self {
        Self {
            ad,
            rk: *sk,
            dhs_pub: PublicKey::from(&spk).to_bytes(),
            dhs: spk,
            dhr: None,
            cks: None,
            ckr: None,
            ns: 0,
            nr: 0,
            pn: 0,
            skipped: SkippedKeys::default(),
            decrypted: Decrypted::default(),
        }
    }

    /// The responder cannot send until the initiator's first message arrives.
    pub fn can_send(&self) -> bool {
        self.cks.is_some()
    }

    pub fn encrypt(
        &mut self,
        plaintext: &[u8],
        extra_aad: &[u8],
    ) -> Result<(Header, Vec<u8>), CryptoError> {
        let cks = self.cks.as_ref().ok_or(CryptoError::NotReady)?;
        let ns_next = self.ns.checked_add(1).ok_or(CryptoError::Malformed)?;
        let (next_ck, mk) = kdf_ck(cks);
        let mk = Zeroizing::new(mk);
        let header = Header {
            dh: self.dhs_pub,
            pn: self.pn,
            n: self.ns,
        };
        replace_key(&mut self.cks, next_ck);
        self.ns = ns_next;

        let keys = message_keys(&mk);
        let ct = aead::seal(
            &keys.key,
            &keys.nonce,
            &self.aad(&header, extra_aad),
            plaintext,
        );
        Ok((header, ct))
    }

    /// Takes over from `older`, the session this one replaces with the same
    /// device, which messages it decrypted, so that a copy of one is a
    /// [`CryptoError::Duplicate`] here too (what this session decrypted
    /// counts as the more recent), and the numbering of skipped keys. Its
    /// skipped keys are dropped: every stored one is spent here
    /// ([`Ratchet::take_changes`]), and this session's own keys are
    /// numbered on from them. Only for the same device, and only for a
    /// session not stored yet: a digest does not name the session it was
    /// read in, and this session's keys get new numbers.
    pub fn inherit(&mut self, older: &mut Self) {
        let newer = std::mem::replace(&mut self.decrypted, std::mem::take(&mut older.decrypted));
        for digest in newer.order {
            self.decrypted.insert(digest);
        }
        let own = std::mem::replace(&mut self.skipped, older.skipped.successor());
        for (key, mk, created_ms) in own.iter() {
            self.skipped.insert(key, *mk, created_ms);
        }
    }

    /// Decrypts a message and moves the session on; keys it sets aside for
    /// messages it skips are dated `now_ms`, and a key kept longer than
    /// [`MAX_SKIPPED_AGE_MS`] opens nothing. On any error the session is
    /// left exactly as it was.
    ///
    /// A byte-for-byte copy of one of the last [`MAX_DECRYPTED`] messages
    /// decrypted fails with [`CryptoError::Duplicate`]: its key is spent, and
    /// it carries nothing new. Only an exact copy does: a message at a spent
    /// position that differs in any byte fails authentication, as it may
    /// come from a peer whose state went back in time, which only a fresh
    /// session repairs.
    pub fn decrypt(
        &mut self,
        header: &Header,
        ciphertext: &[u8],
        extra_aad: &[u8],
        now_ms: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<Vec<u8>, CryptoError> {
        let aad = self.aad(header, extra_aad);
        let digest = message_digest(header, extra_aad, ciphertext);
        if self.decrypted.contains(&digest) {
            return Err(CryptoError::Duplicate);
        }
        let key = (header.dh, header.n);
        if let Some(mk) = self.skipped.get(&key, now_ms) {
            let pt = open(mk, &aad, ciphertext)?;
            self.skipped.remove(&key);
            self.decrypted.insert(digest);
            return Ok(pt);
        }

        let plan = self.plan(header, rng)?;
        let pt = open(&plan.mk, &aad, ciphertext)?;
        self.commit(header, plan, now_ms);
        self.decrypted.insert(digest);
        Ok(pt)
    }

    fn plan(&self, h: &Header, rng: &mut impl CryptoRngCore) -> Result<Plan, CryptoError> {
        let mut skipped = Vec::new();
        let (ckr, nr, step) = if self.dhr == Some(h.dh) {
            let ckr = self.ckr.ok_or(CryptoError::Malformed)?;
            (Zeroizing::new(ckr), self.nr, None)
        } else {
            if let (Some(ck), Some(dhr)) = (self.ckr, self.dhr) {
                skip_chain(Zeroizing::new(ck), self.nr, h.pn, dhr, &mut skipped)?;
            }
            let (rk1, ckr) = kdf_rk(&self.rk, &*dh(&self.dhs, &h.dh)?);
            let rk1 = Zeroizing::new(rk1);
            let dhs = StaticSecret::random_from_rng(rng);
            let (rk2, cks) = kdf_rk(&rk1, &*dh(&dhs, &h.dh)?);
            let step = Step {
                rk: Zeroizing::new(rk2),
                dhs,
                cks: Zeroizing::new(cks),
            };
            (Zeroizing::new(ckr), 0, Some(step))
        };

        if h.n < nr {
            return Err(CryptoError::Aead);
        }
        let ckr = skip_chain(ckr, nr, h.n, h.dh, &mut skipped)?;
        let (next_ck, mk) = kdf_ck(&ckr);
        Ok(Plan {
            step,
            skipped,
            ckr: Zeroizing::new(next_ck),
            nr: h.n.checked_add(1).ok_or(CryptoError::Malformed)?,
            mk: Zeroizing::new(mk),
        })
    }

    fn commit(&mut self, h: &Header, mut plan: Plan, now_ms: u64) {
        for (k, mk) in plan.skipped.drain(..) {
            self.skipped.insert(k, *mk, now_ms);
        }
        if let Some(step) = plan.step.take() {
            self.pn = self.ns;
            self.ns = 0;
            self.dhr = Some(h.dh);
            self.rk.zeroize();
            self.rk = *step.rk;
            self.dhs_pub = PublicKey::from(&step.dhs).to_bytes();
            self.dhs = step.dhs;
            replace_key(&mut self.cks, *step.cks);
        }
        replace_key(&mut self.ckr, *plan.ckr);
        self.nr = plan.nr;
    }

    fn aad(&self, header: &Header, extra: &[u8]) -> Vec<u8> {
        let mut aad = Vec::with_capacity(extra.len().saturating_add(AAD_FIXED_LEN));
        aad.extend_from_slice(AAD_CONTEXT);
        aad.extend_from_slice(&self.ad[0]);
        aad.extend_from_slice(&self.ad[1]);
        aad.extend_from_slice(&header.encode());
        aad.extend_from_slice(extra);
        aad
    }

    /// The session's state without its skipped keys, which are stored one
    /// by one ([`Ratchet::take_changes`]). It names the sequence number the
    /// next skipped key gets, which tells keys stored ahead of a state that
    /// was itself never stored.
    pub fn to_bytes(&self) -> Zeroizing<Vec<u8>> {
        let snapshot = Snapshot {
            version: SNAPSHOT_VERSION,
            ad: self.ad,
            rk: self.rk,
            dhs: self.dhs.to_bytes(),
            dhr: self.dhr,
            cks: self.cks,
            ckr: self.ckr,
            ns: self.ns,
            nr: self.nr,
            pn: self.pn,
            next_skipped: self.skipped.next_seq(),
        };
        #[expect(
            clippy::expect_used,
            reason = "serializing plain arrays and integers into a Vec cannot fail"
        )]
        let bytes = postcard::to_allocvec(&snapshot).expect("in-memory serialization");
        Zeroizing::new(bytes)
    }

    /// A session from its snapshot, without skipped keys: those stored are
    /// taken back by [`Ratchet::restore_skipped`]. A snapshot of version 2
    /// held its keys inside: they come out new, dated `now_ms`, to be
    /// stored ([`Ratchet::take_changes`]), and no stored key is taken back.
    pub fn from_bytes(b: &[u8], now_ms: u64) -> Result<Self, CryptoError> {
        let (state, skipped) = match b.first() {
            Some(&SNAPSHOT_VERSION) => {
                let s: Snapshot = postcard::from_bytes(b).map_err(|_| CryptoError::Malformed)?;
                (s.state(), SkippedKeys::stored_until(s.next_skipped))
            }
            Some(&SNAPSHOT_V2) => {
                let s: SnapshotV2 = postcard::from_bytes(b).map_err(|_| CryptoError::Malformed)?;
                if s.skipped.len() > MAX_STORED_SKIPPED {
                    return Err(CryptoError::Malformed);
                }
                (s.state(), SkippedKeys::unstored(&s.skipped, now_ms))
            }
            _ => return Err(CryptoError::Malformed),
        };
        let dhs = StaticSecret::from(state.dhs);
        Ok(Self {
            ad: state.ad,
            rk: state.rk,
            dhs_pub: PublicKey::from(&dhs).to_bytes(),
            dhs,
            dhr: state.dhr,
            cks: state.cks,
            ckr: state.ckr,
            ns: state.ns,
            nr: state.nr,
            pn: state.pn,
            skipped,
            decrypted: Decrypted::default(),
        })
    }

    /// Takes a skipped key stored before back in. Refused, to be deleted,
    /// when the snapshot this session came from does not count it as
    /// stored (it was stored ahead of a state that never was), or it is a
    /// copy. The oldest beyond [`MAX_STORED_SKIPPED`] are dropped.
    pub fn restore_skipped(&mut self, key: &SkippedKey) -> bool {
        self.skipped.restore(key)
    }

    /// What changed in the skipped keys since the last call: what to store
    /// before the snapshot that counts it, and what to delete after it.
    pub fn take_changes(&mut self) -> Changes {
        self.skipped.take_changes()
    }

    /// The sequence numbers of every skipped key stored for this session,
    /// whether still kept or gone since: what deleting it deletes.
    pub fn stored_skipped(&self) -> impl Iterator<Item = u64> + '_ {
        self.skipped.stored_seqs()
    }

    /// Drops the skipped keys kept longer than [`MAX_SKIPPED_AGE_MS`] at
    /// `now_ms`; whether there were any, which then are to be deleted
    /// ([`Ratchet::take_changes`]).
    pub fn expire_skipped(&mut self, now_ms: u64) -> bool {
        self.skipped.expire(now_ms)
    }
}

impl std::fmt::Debug for Ratchet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ratchet")
            .field("can_send", &self.can_send())
            .field("ns", &self.ns)
            .field("nr", &self.nr)
            .field("pn", &self.pn)
            .field("skipped", &self.skipped.len())
            .finish_non_exhaustive()
    }
}

impl Drop for Ratchet {
    fn drop(&mut self) {
        self.rk.zeroize();
        self.cks.zeroize();
        self.ckr.zeroize();
    }
}

/// What a snapshot holds of a session besides its skipped keys.
struct State {
    ad: [[u8; 32]; 2],
    rk: [u8; 32],
    dhs: [u8; 32],
    dhr: Option<[u8; 32]>,
    cks: Option<[u8; 32]>,
    ckr: Option<[u8; 32]>,
    ns: u32,
    nr: u32,
    pn: u32,
}

impl Drop for State {
    fn drop(&mut self) {
        self.rk.zeroize();
        self.dhs.zeroize();
        self.cks.zeroize();
        self.ckr.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u8,
    ad: [[u8; 32]; 2],
    rk: [u8; 32],
    dhs: [u8; 32],
    dhr: Option<[u8; 32]>,
    cks: Option<[u8; 32]>,
    ckr: Option<[u8; 32]>,
    ns: u32,
    nr: u32,
    pn: u32,
    next_skipped: u64,
}

impl Snapshot {
    fn state(&self) -> State {
        State {
            ad: self.ad,
            rk: self.rk,
            dhs: self.dhs,
            dhr: self.dhr,
            cks: self.cks,
            ckr: self.ckr,
            ns: self.ns,
            nr: self.nr,
            pn: self.pn,
        }
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        self.rk.zeroize();
        self.dhs.zeroize();
        self.cks.zeroize();
        self.ckr.zeroize();
    }
}

/// [`Snapshot`] as version 2 stored it, skipped keys inside.
#[derive(Deserialize)]
struct SnapshotV2 {
    #[expect(dead_code, reason = "read to tell the version only")]
    version: u8,
    ad: [[u8; 32]; 2],
    rk: [u8; 32],
    dhs: [u8; 32],
    dhr: Option<[u8; 32]>,
    cks: Option<[u8; 32]>,
    ckr: Option<[u8; 32]>,
    ns: u32,
    nr: u32,
    pn: u32,
    skipped: Vec<(SkipKey, [u8; 32])>,
}

impl SnapshotV2 {
    fn state(&self) -> State {
        State {
            ad: self.ad,
            rk: self.rk,
            dhs: self.dhs,
            dhr: self.dhr,
            cks: self.cks,
            ckr: self.ckr,
            ns: self.ns,
            nr: self.nr,
            pn: self.pn,
        }
    }
}

impl Drop for SnapshotV2 {
    fn drop(&mut self) {
        self.rk.zeroize();
        self.dhs.zeroize();
        self.cks.zeroize();
        self.ckr.zeroize();
        self.skipped.iter_mut().for_each(|(_, mk)| mk.zeroize());
    }
}

fn replace_key(slot: &mut Option<[u8; 32]>, new: [u8; 32]) {
    slot.zeroize();
    *slot = Some(new);
}

pub(crate) fn dh(
    secret: &StaticSecret,
    public: &[u8; 32],
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    let shared = secret.diffie_hellman(&PublicKey::from(*public));
    if !shared.was_contributory() {
        return Err(CryptoError::NonContributory);
    }
    Ok(Zeroizing::new(shared.to_bytes()))
}

fn skip_chain(
    mut ck: Zeroizing<[u8; 32]>,
    from: u32,
    until: u32,
    dh: [u8; 32],
    out: &mut Vec<(SkipKey, Zeroizing<[u8; 32]>)>,
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    if until <= from {
        return Ok(ck);
    }
    let new_keys = usize::try_from(until.saturating_sub(from)).unwrap_or(usize::MAX);
    if new_keys.saturating_add(out.len()) > MAX_SKIP as usize {
        return Err(CryptoError::TooManySkipped);
    }
    for n in from..until {
        let (next, mk) = kdf_ck(&ck);
        out.push(((dh, n), Zeroizing::new(mk)));
        *ck = next;
    }
    Ok(ck)
}

/// What tells one message from any other: a hash of what travels of it
/// (header and ciphertext) and the associated data the caller adds. The
/// session's own identities are left out, so that a session that replaces
/// another with the same device still knows its copies; the identities
/// order differently depending on which side started the session. Not
/// secret: all of it is in the clear.
fn message_digest(header: &Header, extra_aad: &[u8], ciphertext: &[u8]) -> [u8; 32] {
    let extra_len = u64::try_from(extra_aad.len()).unwrap_or(u64::MAX);
    Sha256::new()
        .chain_update(header.encode())
        .chain_update(extra_len.to_le_bytes())
        .chain_update(extra_aad)
        .chain_update(ciphertext)
        .finalize()
        .into()
}

fn open(mk: &[u8; 32], aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let keys = message_keys(mk);
    aead::open(&keys.key, &keys.nonce, aad, ciphertext)
}

#[cfg(test)]
mod tests {
    use rand::rngs::OsRng;
    use serde::Serialize;

    use super::*;

    /// [`SnapshotV2`] as that version wrote it.
    #[derive(Serialize)]
    struct V2 {
        version: u8,
        ad: [[u8; 32]; 2],
        rk: [u8; 32],
        dhs: [u8; 32],
        dhr: Option<[u8; 32]>,
        cks: Option<[u8; 32]>,
        ckr: Option<[u8; 32]>,
        ns: u32,
        nr: u32,
        pn: u32,
        skipped: Vec<(SkipKey, [u8; 32])>,
    }

    fn as_version_2(r: &Ratchet) -> Vec<u8> {
        postcard::to_allocvec(&V2 {
            version: SNAPSHOT_V2,
            ad: r.ad,
            rk: r.rk,
            dhs: r.dhs.to_bytes(),
            dhr: r.dhr,
            cks: r.cks,
            ckr: r.ckr,
            ns: r.ns,
            nr: r.nr,
            pn: r.pn,
            skipped: r.skipped.iter().map(|(k, mk, _)| (k, *mk)).collect(),
        })
        .unwrap()
    }

    /// A session stored before its skipped keys were kept apart: its keys
    /// come out new, numbered from the start and dated by the clock it is
    /// read at, to be stored; no stored key is taken back; and they open
    /// their messages.
    #[test]
    fn a_version_2_snapshot_gives_its_keys_out_to_store() {
        let (sk, ad) = ([1; 32], [[2; 32], [3; 32]]);
        let spk = StaticSecret::random_from_rng(OsRng);
        let spk_pub = PublicKey::from(&spk).to_bytes();
        let mut a = Ratchet::init_initiator(&sk, ad, &spk_pub, &mut OsRng).unwrap();
        let mut b = Ratchet::init_responder(&sk, ad, spk);
        let late: Vec<_> = (0..3).map(|_| a.encrypt(b"late", b"").unwrap()).collect();
        let (h, ct) = a.encrypt(b"on time", b"").unwrap();
        b.decrypt(&h, &ct, b"", 5, &mut OsRng).unwrap();

        let mut read = Ratchet::from_bytes(&as_version_2(&b), 77).unwrap();
        let stored = SkippedKey::new(0, late[0].0.dh, 0, [9; 32], 1);
        assert!(
            !read.restore_skipped(&stored),
            "nothing stored to take back"
        );
        let changes = read.take_changes();
        let seqs: Vec<u64> = changes.stored.iter().map(SkippedKey::seq).collect();
        assert_eq!(seqs, [0, 1, 2]);
        assert!(changes.stored.iter().all(|k| k.created_ms() == 77));
        assert!(changes.spent.is_empty());
        assert_eq!(read.to_bytes().first(), Some(&SNAPSHOT_VERSION));
        for (h, ct) in late.iter().rev() {
            assert_eq!(read.decrypt(h, ct, b"", 80, &mut OsRng).unwrap(), b"late");
        }
        assert_eq!(read.take_changes().spent, [2, 1, 0]);
    }
}
