//! Messages this identity's other devices sent to contacts this device does
//! not know yet. A device just linked hears of what its siblings send
//! before their contacts reach it; such a record waits here, briefly and
//! within bounds, and is stored as usual once its contact is known.
//!
//! Only our own devices tell of messages sent ([`super::Core::on_sync`] is
//! reached only for them), so no one else can fill this.
//!
//! Each record is its own row in `meta` (`META_HELD ‖ seq (BE)`), written
//! once when it is put aside and deleted when it leaves: keeping one costs
//! the record's size, not that of all the others.

use std::collections::VecDeque;

use cypher_types::PeerId;
use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};

use super::messaging::MAX_CLOCK_SKEW_MS;
use super::{Core, Rows, Skipped};
use crate::CoreError;
use crate::api::{Content, MediaKind, StoredMessage};
use crate::store::{META_HELD, StoreOp, Table, Vault};

/// Records kept at most; a new one past this lets the oldest go. Far more
/// than a user sends while a new device sets up.
const MAX_HELD: usize = 128;
/// What the records kept weigh at most ([`weight`]). A record weighs at
/// most about 17 KiB (a text or a poster), so the newest one always stays.
const MAX_HELD_BYTES: usize = 256 * 1024;
/// How long a record waits for its contact. The siblings answer a new
/// device as soon as they hear it, so a day leaves room for one that is
/// closed meanwhile; the contact not known by then is not coming.
const HELD_TTL_MS: u64 = 24 * 60 * 60_000;
/// What a record weighs besides the strings and bytes it carries.
const RECORD_OVERHEAD: usize = 96;

/// One message sent, and when it was put aside: the row of one record.
#[derive(Debug, Serialize, Deserialize)]
struct Held {
    at_ms: u64,
    msg: StoredMessage,
}

impl crate::Record for Held {
    /// Holds a [`StoredMessage`] as it is: a new version of that makes a
    /// new one of this.
    const VERSION: u8 = 1;
}

/// Where record `seq` is kept.
fn held_key(seq: u64) -> Vec<u8> {
    let mut key = META_HELD.to_vec();
    key.extend_from_slice(&seq.to_be_bytes());
    key
}

/// Roughly what a record takes, in bytes.
fn weight(msg: &StoredMessage) -> usize {
    let carried = match &msg.content {
        Content::Text { text, .. } => text.len(),
        Content::File {
            name, mime, kind, ..
        } => {
            let media = match kind {
                MediaKind::File => 0,
                MediaKind::Voice { waveform, .. } => waveform.len(),
                MediaKind::VideoNote { poster, .. } => poster.len(),
            };
            name.len().saturating_add(mime.len()).saturating_add(media)
        }
    };
    carried.saturating_add(RECORD_OVERHEAD)
}

/// The records waiting for their contacts, oldest first, each with the
/// number of its row.
#[derive(Debug, Default)]
pub(super) struct HeldSent {
    held: VecDeque<(u64, Held)>,
    /// What the records weigh together.
    bytes: usize,
    /// The number the next record's row takes.
    next_seq: u64,
}

impl HeldSent {
    /// The records kept in `meta`, as this device left them.
    pub(super) fn load(
        vault: &Vault,
        meta: &Rows,
        skipped: &mut Skipped,
    ) -> Result<Self, CoreError> {
        let mut rows: Vec<(u64, Held)> = Vec::new();
        for (key, value) in meta {
            let Some(seq) = key
                .strip_prefix(META_HELD)
                .and_then(|s| <[u8; 8]>::try_from(s).ok())
                .map(u64::from_be_bytes)
            else {
                continue;
            };
            if let Some(held) = skipped.open::<Held>(vault, Table::Meta, key, value)? {
                rows.push((seq, held));
            }
        }
        rows.sort_unstable_by_key(|(seq, _)| *seq);
        let next_seq = rows.last().map_or(0, |(seq, _)| seq.saturating_add(1));
        let bytes = rows.iter().map(|(_, h)| weight(&h.msg)).sum();
        Ok(Self {
            held: rows.into(),
            bytes,
            next_seq,
        })
    }

    pub(super) fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Takes record `i` out.
    fn remove(&mut self, i: usize) -> Option<(u64, Held)> {
        let removed = self.held.remove(i)?;
        self.bytes = self.bytes.saturating_sub(weight(&removed.1.msg));
        Some(removed)
    }

    /// Puts `msg` aside at `now`, last, in place of an earlier copy of it;
    /// the oldest records go while there are too many. The numbers of the
    /// records gone.
    fn hold(&mut self, msg: StoredMessage, now: u64) -> Vec<u64> {
        let mut gone = Vec::new();
        if let Some(i) = self
            .held
            .iter()
            .position(|(_, h)| h.msg.msg_id == msg.msg_id)
            && let Some((seq, _)) = self.remove(i)
        {
            gone.push(seq);
        }
        let seq = self.next_seq;
        self.next_seq = seq.saturating_add(1);
        self.bytes = self.bytes.saturating_add(weight(&msg));
        self.held.push_back((seq, Held { at_ms: now, msg }));
        while self.held.len() > MAX_HELD || self.bytes > MAX_HELD_BYTES {
            let Some((old, _)) = self.remove(0) else {
                break;
            };
            gone.push(old);
        }
        gone
    }

    /// Takes out the records `leaves` picks, oldest first.
    fn take(&mut self, leaves: impl Fn(&Held) -> bool) -> Vec<(u64, StoredMessage)> {
        if !self.held.iter().any(|(_, h)| leaves(h)) {
            return Vec::new();
        }
        let mut taken = Vec::new();
        let mut kept = VecDeque::with_capacity(self.held.len());
        for (seq, held) in self.held.drain(..) {
            if leaves(&held) {
                self.bytes = self.bytes.saturating_sub(weight(&held.msg));
                taken.push((seq, held.msg));
            } else {
                kept.push_back((seq, held));
            }
        }
        self.held = kept;
        taken
    }

    /// Takes out the records that waited too long by `now`.
    fn expire(&mut self, now: u64) -> Vec<(u64, StoredMessage)> {
        self.take(|h| now.saturating_sub(h.at_ms) >= HELD_TTL_MS)
    }

    /// Takes out the records whose contact is now `known`, oldest first.
    fn take_known(&mut self, known: impl Fn(&PeerId) -> bool) -> Vec<(u64, StoredMessage)> {
        self.take(|h| known(&h.msg.peer))
    }

    /// Takes out the records for `peer`.
    fn forget(&mut self, peer: &PeerId) -> Vec<(u64, StoredMessage)> {
        self.take(|h| h.msg.peer == *peer)
    }
}

impl<R: CryptoRngCore> Core<R> {
    /// A message another device of ours sent to a contact this one does not
    /// know yet: kept until it does, dated as it would be stored now.
    pub(super) fn hold_sent(&mut self, mut msg: StoredMessage) {
        msg.sent_at_ms = msg
            .sent_at_ms
            .min(self.now.saturating_add(MAX_CLOCK_SKEW_MS));
        let gone = self.held.hold(msg, self.now);
        if let Some((seq, held)) = self.held.held.back() {
            let op = self
                .vault
                .put(Table::Meta, held_key(*seq), held, &mut self.rng);
            self.persist(op);
        }
        self.delete_held(gone);
    }

    /// Stores the records whose contact this device now knows, however it
    /// came to, as if they had come after it; lets go of those that waited
    /// too long. Each record is stored before its row is deleted: should
    /// the app stop in between, it is stored once more, the same.
    pub(super) fn place_held(&mut self) {
        if self.held.is_empty() {
            return;
        }
        let expired = self.held.expire(self.now);
        self.delete_held(expired.into_iter().map(|(seq, _)| seq));
        let peers = &self.peers;
        let placed = self.held.take_known(|p| peers.contains_key(p));
        let mut done = Vec::with_capacity(placed.len());
        for (seq, msg) in placed {
            let (peer, msg_id) = (msg.peer, msg.msg_id);
            self.store_message(msg);
            self.await_receipts(peer, msg_id);
            done.push(seq);
        }
        self.delete_held(done);
    }

    /// `peer` was forgotten on another device: what it sent them is not
    /// shown here should they come back.
    pub(super) fn forget_held(&mut self, peer: &PeerId) {
        let gone = self.held.forget(peer);
        self.delete_held(gone.into_iter().map(|(seq, _)| seq));
    }

    fn delete_held(&mut self, seqs: impl IntoIterator<Item = u64>) {
        for seq in seqs {
            self.persist(StoreOp::Delete {
                table: Table::Meta,
                key: held_key(seq),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use cypher_types::MsgId;

    use super::*;
    use crate::api::MessageStatus;

    fn sent(peer: u8, id: u8, text_len: usize) -> StoredMessage {
        StoredMessage {
            msg_id: MsgId([id; MsgId::LEN]),
            peer: PeerId([peer; 32]),
            outgoing: true,
            sent_at_ms: 1,
            status: MessageStatus::Sent,
            content: Content::Text {
                text: "x".repeat(text_len),
                reply_to: None,
            },
        }
    }

    fn ids(held: &HeldSent) -> Vec<u8> {
        held.held.iter().map(|(_, h)| h.msg.msg_id.0[0]).collect()
    }

    fn taken_ids(taken: &[(u64, StoredMessage)]) -> Vec<u8> {
        taken.iter().map(|(_, m)| m.msg_id.0[0]).collect()
    }

    /// The running total is what the records weigh.
    fn total_is_kept(held: &HeldSent) {
        let total: usize = held.held.iter().map(|(_, h)| weight(&h.msg)).sum();
        assert_eq!(held.bytes, total);
    }

    #[test]
    fn past_the_count_the_oldest_goes_first() {
        let mut held = HeldSent::default();
        for n in 0..MAX_HELD {
            assert!(
                held.hold(sent(1, u8::try_from(n).unwrap(), 1), 0)
                    .is_empty()
            );
        }
        let gone = held.hold(sent(1, u8::try_from(MAX_HELD).unwrap(), 1), 0);
        assert_eq!(gone, [0], "the first one's row goes");
        let last = held.held.back().map(|(seq, _)| *seq);
        assert_eq!(last, Some(u64::try_from(MAX_HELD).unwrap()));
        assert_eq!(held.held.len(), MAX_HELD);
        assert_eq!(ids(&held).first(), Some(&1));
        total_is_kept(&held);
    }

    #[test]
    fn past_the_bytes_the_oldest_goes_first_and_the_newest_stays() {
        let mut held = HeldSent::default();
        let big = crate::envelope::MAX_TEXT_LEN;
        let fit = MAX_HELD_BYTES / (big + RECORD_OVERHEAD);
        for n in 0..=fit {
            held.hold(sent(1, u8::try_from(n).unwrap(), big), 0);
        }
        assert!(held.bytes <= MAX_HELD_BYTES);
        assert_eq!(held.held.len(), fit);
        assert_eq!(ids(&held).first(), Some(&1), "the first one went");
        assert_eq!(ids(&held).last(), Some(&u8::try_from(fit).unwrap()));
        total_is_kept(&held);
    }

    #[test]
    fn a_copy_takes_the_place_of_the_first() {
        let mut held = HeldSent::default();
        held.hold(sent(1, 7, 1), 0);
        held.hold(sent(1, 8, 1), 0);
        assert_eq!(held.hold(sent(1, 7, 1), 5), [0]);
        assert_eq!(held.held.back().map(|(seq, _)| *seq), Some(2));
        assert_eq!(ids(&held), [8, 7]);
        total_is_kept(&held);
    }

    #[test]
    fn records_wait_a_day_by_the_clock_they_were_held_at() {
        let mut held = HeldSent::default();
        held.hold(sent(1, 1, 1), 1_000);
        held.hold(sent(1, 2, 1), 2_000);
        assert!(held.expire(1_000 + HELD_TTL_MS - 1).is_empty());
        assert_eq!(taken_ids(&held.expire(1_000 + HELD_TTL_MS)), [1]);
        assert_eq!(ids(&held), [2]);
        assert!(held.expire(1_500).is_empty(), "a clock set back keeps them");
        assert_eq!(taken_ids(&held.expire(2_000 + HELD_TTL_MS)), [2]);
        assert!(held.is_empty());
        total_is_kept(&held);
    }

    #[test]
    fn records_leave_once_their_contact_is_known_or_forgotten() {
        let mut held = HeldSent::default();
        held.hold(sent(1, 1, 1), 0);
        held.hold(sent(2, 2, 1), 0);
        held.hold(sent(1, 3, 1), 0);
        held.hold(sent(3, 4, 1), 0);
        assert!(held.take_known(|p| *p == PeerId([9; 32])).is_empty());
        let placed = held.take_known(|p| *p == PeerId([1; 32]));
        assert_eq!(taken_ids(&placed), [1, 3], "oldest first");
        assert_eq!(placed.iter().map(|(s, _)| *s).collect::<Vec<_>>(), [0, 2]);
        assert_eq!(ids(&held), [2, 4]);
        assert_eq!(taken_ids(&held.forget(&PeerId([2; 32]))), [2]);
        assert!(held.forget(&PeerId([2; 32])).is_empty());
        assert_eq!(ids(&held), [4]);
        total_is_kept(&held);
    }
}
