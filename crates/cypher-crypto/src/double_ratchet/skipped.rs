//! Message keys kept for messages that have not arrived yet, each stored
//! apart from the ratchet state under a sequence number of its own: taking
//! one or adding a few changes only those, whatever else is kept.

use std::collections::{BTreeMap, HashMap};

use zeroize::Zeroize;

/// Most skipped keys retained per session; the oldest are evicted first.
pub const MAX_STORED_SKIPPED: usize = 2000;

/// Which message a key opens: the sender's ratchet key and the message's
/// number on that chain.
pub(super) type SkipKey = ([u8; 32], u32);

/// One key kept for a message that has not arrived yet, as stored: the
/// message it opens, when it was set aside (the caller's clock), and the
/// sequence number it is stored under, unique within its session and its
/// successors ([`super::Ratchet::inherit`]).
pub struct SkippedKey {
    seq: u64,
    dh: [u8; 32],
    n: u32,
    mk: [u8; 32],
    created_ms: u64,
}

impl SkippedKey {
    pub fn new(seq: u64, dh: [u8; 32], n: u32, mk: [u8; 32], created_ms: u64) -> Self {
        Self {
            seq,
            dh,
            n,
            mk,
            created_ms,
        }
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// The sender's ratchet key of the chain the message is on.
    pub fn dh(&self) -> &[u8; 32] {
        &self.dh
    }

    /// The message's number on its chain.
    pub fn n(&self) -> u32 {
        self.n
    }

    /// The message key itself.
    pub fn mk(&self) -> &[u8; 32] {
        &self.mk
    }

    pub fn created_ms(&self) -> u64 {
        self.created_ms
    }
}

impl std::fmt::Debug for SkippedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkippedKey")
            .field("seq", &self.seq)
            .field("n", &self.n)
            .field("created_ms", &self.created_ms)
            .finish_non_exhaustive()
    }
}

impl Drop for SkippedKey {
    fn drop(&mut self) {
        self.mk.zeroize();
    }
}

/// What changed in a session's skipped keys since they were last stored:
/// keys to store, and the sequence numbers of stored keys that are gone
/// (used, evicted, expired, or dropped with a session replaced).
#[derive(Debug, Default)]
pub struct Changes {
    pub stored: Vec<SkippedKey>,
    pub spent: Vec<u64>,
}

/// A kept key, by its sequence number.
struct Entry {
    key: SkipKey,
    mk: [u8; 32],
    created_ms: u64,
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.mk.zeroize();
    }
}

/// The keys kept, oldest first, and what of them is stored. Every key from
/// `stored_upto` on is new since the keys were last stored; every one
/// below it is stored, and `spent` names those stored that are gone.
#[derive(Default)]
pub(super) struct SkippedKeys {
    by_seq: BTreeMap<u64, Entry>,
    index: HashMap<SkipKey, u64>,
    /// The sequence number of the next key set aside.
    next_seq: u64,
    stored_upto: u64,
    spent: Vec<u64>,
}

impl SkippedKeys {
    /// Keys stored up to `next_seq`, none of them loaded yet.
    pub(super) fn stored_until(next_seq: u64) -> Self {
        Self {
            next_seq,
            stored_upto: next_seq,
            ..Self::default()
        }
    }

    pub(super) fn len(&self) -> usize {
        self.by_seq.len()
    }

    pub(super) fn next_seq(&self) -> u64 {
        self.next_seq
    }

    pub(super) fn get(&self, k: &SkipKey) -> Option<&[u8; 32]> {
        let seq = self.index.get(k)?;
        self.by_seq.get(seq).map(|e| &e.mk)
    }

    pub(super) fn remove(&mut self, k: &SkipKey) {
        if let Some(seq) = self.index.remove(k) {
            self.drop_seq(seq);
        }
    }

    fn drop_seq(&mut self, seq: u64) {
        if let Some(entry) = self.by_seq.remove(&seq) {
            self.index.remove(&entry.key);
            if seq < self.stored_upto {
                self.spent.push(seq);
            }
        }
    }

    /// Sets a key aside under the next sequence number, evicting the
    /// oldest beyond [`MAX_STORED_SKIPPED`].
    pub(super) fn insert(&mut self, key: SkipKey, mk: [u8; 32], created_ms: u64) {
        if self.index.contains_key(&key) {
            return;
        }
        while self.by_seq.len() >= MAX_STORED_SKIPPED {
            let Some(oldest) = self.by_seq.keys().next().copied() else {
                break;
            };
            self.drop_seq(oldest);
        }
        let seq = self.next_seq;
        // A session sets aside at most `u64::MAX` keys in its lifetime.
        self.next_seq = seq.saturating_add(1);
        self.index.insert(key, seq);
        self.by_seq.insert(
            seq,
            Entry {
                key,
                mk,
                created_ms,
            },
        );
    }

    /// Takes a stored key back in; refused when it was never stored (its
    /// number is not below what was stored), or its number or message is
    /// taken already. The oldest beyond [`MAX_STORED_SKIPPED`] are evicted.
    pub(super) fn restore(&mut self, stored: &SkippedKey) -> bool {
        let key = (stored.dh, stored.n);
        if stored.seq >= self.stored_upto
            || self.by_seq.contains_key(&stored.seq)
            || self.index.contains_key(&key)
        {
            return false;
        }
        self.index.insert(key, stored.seq);
        self.by_seq.insert(
            stored.seq,
            Entry {
                key,
                mk: stored.mk,
                created_ms: stored.created_ms,
            },
        );
        while self.by_seq.len() > MAX_STORED_SKIPPED {
            let Some(oldest) = self.by_seq.keys().next().copied() else {
                break;
            };
            self.drop_seq(oldest);
        }
        true
    }

    /// What changed since the last call; everything kept counts as stored
    /// from now on.
    pub(super) fn take_changes(&mut self) -> Changes {
        let stored = self
            .by_seq
            .range(self.stored_upto..)
            .map(|(&seq, e)| SkippedKey::new(seq, e.key.0, e.key.1, e.mk, e.created_ms))
            .collect();
        self.stored_upto = self.next_seq;
        Changes {
            stored,
            spent: std::mem::take(&mut self.spent),
        }
    }

    /// Every stored key this one knows of, kept or gone since last stored:
    /// what deleting the session must delete.
    pub(super) fn stored_seqs(&self) -> impl Iterator<Item = u64> + '_ {
        self.by_seq
            .range(..self.stored_upto)
            .map(|(&seq, _)| seq)
            .chain(self.spent.iter().copied())
    }

    /// The keys a session that replaces this one starts from: none, but
    /// numbered on from these, with every stored one of these to delete.
    pub(super) fn successor(&mut self) -> Self {
        let mut spent = std::mem::take(&mut self.spent);
        spent.extend(self.by_seq.range(..self.stored_upto).map(|(&seq, _)| seq));
        Self {
            next_seq: self.next_seq,
            stored_upto: self.stored_upto,
            spent,
            ..Self::default()
        }
    }

    /// The keys kept, oldest first.
    pub(super) fn iter(&self) -> impl Iterator<Item = (SkipKey, &[u8; 32], u64)> + '_ {
        self.by_seq.values().map(|e| (e.key, &e.mk, e.created_ms))
    }

    /// Every key, new or stored, as if none were stored yet: a session
    /// read from a snapshot that held its keys inside.
    pub(super) fn unstored(keys: &[(SkipKey, [u8; 32])], created_ms: u64) -> Self {
        let mut out = Self::default();
        for (key, mk) in keys {
            out.insert(*key, *mk, created_ms);
        }
        out
    }
}
