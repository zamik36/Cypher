//! Message keys kept for messages that have not arrived yet, each stored
//! apart from the ratchet state under a sequence number of its own: taking
//! one or adding a few changes only those, whatever else is kept.

use std::collections::{BTreeMap, HashMap};

use zeroize::Zeroize;

/// Most skipped keys retained per session; the oldest are evicted first.
pub const MAX_STORED_SKIPPED: usize = 2000;

/// How long a skipped key is kept after it was set aside: 30 days.
///
/// A key is set aside when a later message on its chain arrives, so the
/// message it opens was sent before then. Every way a message travels ends
/// within 14 days of its sending: online delivery is immediate, and an
/// inbox item lives 14 days at most (`INBOX_TTL_SECS` in signaling). Twice
/// that leaves room for the device's clock being off. A key kept longer
/// could only open a message that will not come, while it stays on the
/// device for anyone who takes it to read that message from a copy made
/// on the way.
pub const MAX_SKIPPED_AGE_MS: u64 = 30 * 24 * 3600 * 1000;

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

/// A kept key, by its sequence number.
struct Entry {
    key: SkipKey,
    mk: [u8; 32],
    created_ms: u64,
}

impl Entry {
    /// Whether the key outlived [`MAX_SKIPPED_AGE_MS`] at `now_ms`; a key
    /// dated ahead of the clock has not.
    fn expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.created_ms) >= MAX_SKIPPED_AGE_MS
    }
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

    /// The key for message `k`, unless it outlived [`MAX_SKIPPED_AGE_MS`]
    /// at `now_ms`.
    pub(super) fn get(&self, k: &SkipKey, now_ms: u64) -> Option<&[u8; 32]> {
        let seq = self.index.get(k)?;
        self.by_seq
            .get(seq)
            .filter(|e| !e.expired(now_ms))
            .map(|e| &e.mk)
    }

    /// Drops every key that outlived [`MAX_SKIPPED_AGE_MS`] at `now_ms`;
    /// whether there was one. Keys are dated as they are set aside, oldest
    /// first, so a look at the oldest tells, unless a clock went back.
    pub(super) fn expire(&mut self, now_ms: u64) -> bool {
        if !self
            .by_seq
            .values()
            .next()
            .is_some_and(|oldest| oldest.expired(now_ms))
        {
            return false;
        }
        let expired: Vec<u64> = self
            .by_seq
            .iter()
            .filter(|(_, e)| e.expired(now_ms))
            .map(|(&seq, _)| seq)
            .collect();
        for seq in expired {
            self.drop_seq(seq);
        }
        true
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

    /// What changed since the last call: keys to store, and the numbers of
    /// stored keys that are gone. Everything kept counts as stored from now
    /// on.
    pub(super) fn take_changes(&mut self) -> (Vec<SkippedKey>, Vec<u64>) {
        let stored = self
            .by_seq
            .range(self.stored_upto..)
            .map(|(&seq, e)| SkippedKey::new(seq, e.key.0, e.key.1, e.mk, e.created_ms))
            .collect();
        self.stored_upto = self.next_seq;
        (stored, std::mem::take(&mut self.spent))
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
