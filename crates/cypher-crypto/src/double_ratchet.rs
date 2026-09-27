use std::collections::{HashMap, VecDeque};

use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, Zeroizing};

use crate::aead;
use crate::error::CryptoError;
use crate::kdf::{kdf_ck, kdf_rk, message_keys};

pub const HEADER_LEN: usize = 40;

/// Most message keys a single incoming message may force us to derive.
pub const MAX_SKIP: u32 = 1000;

/// Most skipped keys retained per session; the oldest are evicted first.
const MAX_STORED_SKIPPED: usize = 2000;

const SNAPSHOT_VERSION: u8 = 2;
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

type SkipKey = ([u8; 32], u32);

#[derive(Default)]
struct SkippedKeys {
    keys: HashMap<SkipKey, [u8; 32]>,
    order: VecDeque<SkipKey>,
}

impl SkippedKeys {
    fn get(&self, k: &SkipKey) -> Option<&[u8; 32]> {
        self.keys.get(k)
    }

    fn remove(&mut self, k: &SkipKey) {
        if let Some(mut mk) = self.keys.remove(k) {
            mk.zeroize();
        }
    }

    fn insert(&mut self, k: SkipKey, mk: [u8; 32]) {
        while self.keys.len() >= MAX_STORED_SKIPPED {
            match self.order.pop_front() {
                Some(oldest) => self.remove(&oldest),
                None => break,
            }
        }
        if self.order.len() >= 2 * MAX_STORED_SKIPPED {
            let keys = &self.keys;
            self.order.retain(|k| keys.contains_key(k));
        }
        self.keys.insert(k, mk);
        self.order.push_back(k);
    }

    fn iter_ordered(&self) -> impl Iterator<Item = (SkipKey, [u8; 32])> + '_ {
        self.order
            .iter()
            .filter_map(|k| self.keys.get(k).map(|mk| (*k, *mk)))
    }
}

impl Drop for SkippedKeys {
    fn drop(&mut self) {
        self.keys.values_mut().for_each(Zeroize::zeroize);
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
    skipped: SkippedKeys,
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

    pub fn decrypt(
        &mut self,
        header: &Header,
        ciphertext: &[u8],
        extra_aad: &[u8],
        rng: &mut impl CryptoRngCore,
    ) -> Result<Vec<u8>, CryptoError> {
        let aad = self.aad(header, extra_aad);
        let key = (header.dh, header.n);
        if let Some(mk) = self.skipped.get(&key) {
            let pt = open(mk, &aad, ciphertext)?;
            self.skipped.remove(&key);
            return Ok(pt);
        }

        let plan = self.plan(header, rng)?;
        let pt = open(&plan.mk, &aad, ciphertext)?;
        self.commit(header, plan);
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

    fn commit(&mut self, h: &Header, mut plan: Plan) {
        for (k, mk) in plan.skipped.drain(..) {
            self.skipped.insert(k, *mk);
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
            skipped: self.skipped.iter_ordered().collect(),
        };
        #[expect(
            clippy::expect_used,
            reason = "serializing plain arrays and integers into a Vec cannot fail"
        )]
        let bytes = postcard::to_allocvec(&snapshot).expect("in-memory serialization");
        Zeroizing::new(bytes)
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self, CryptoError> {
        let s: Snapshot = postcard::from_bytes(b).map_err(|_| CryptoError::Malformed)?;
        if s.version != SNAPSHOT_VERSION || s.skipped.len() > MAX_STORED_SKIPPED {
            return Err(CryptoError::Malformed);
        }
        let dhs = StaticSecret::from(s.dhs);
        let mut skipped = SkippedKeys::default();
        for (k, mk) in &s.skipped {
            skipped.insert(*k, *mk);
        }
        Ok(Self {
            ad: s.ad,
            rk: s.rk,
            dhs_pub: PublicKey::from(&dhs).to_bytes(),
            dhs,
            dhr: s.dhr,
            cks: s.cks,
            ckr: s.ckr,
            ns: s.ns,
            nr: s.nr,
            pn: s.pn,
            skipped,
        })
    }
}

impl std::fmt::Debug for Ratchet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ratchet")
            .field("can_send", &self.can_send())
            .field("ns", &self.ns)
            .field("nr", &self.nr)
            .field("pn", &self.pn)
            .field("skipped", &self.skipped.keys.len())
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
    skipped: Vec<(SkipKey, [u8; 32])>,
}

impl Drop for Snapshot {
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

fn open(mk: &[u8; 32], aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let keys = message_keys(mk);
    aead::open(&keys.key, &keys.nonce, aad, ciphertext)
}
