//! Chunked transfer state machines. Pure logic: no IO, no clocks.

use std::collections::BTreeMap;

use cypher_crypto::chunk::CHUNK_TAG_LEN;
use cypher_crypto::{ChunkCipher, FileKey};
use cypher_types::{Addr, DeviceId, PeerId};
use serde::{Deserialize, Serialize};

use crate::api::MediaKind;
use crate::envelope::FileDesc;

const WINDOW_BYTES: u64 = 8 << 20;
const MIN_WINDOW: usize = 4;
const MAX_WINDOW: usize = 256;
pub(crate) const RTO_MS: u64 = 4_000;
pub(crate) const MAX_RETRIES: u8 = 5;
/// Receiver acknowledges at least this often (in chunks).
const ACK_EVERY: u32 = 8;

/// Fixed-size bitset with O(1) population count and an amortised O(1)
/// "all set below" prefix, so per-chunk operations stay cheap on huge files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Bitmap {
    words: Vec<u64>,
    len: u32,
    ones: u32,
    prefix: u32,
}

impl Bitmap {
    pub(crate) fn new(len: u32) -> Self {
        Self {
            words: vec![0; (len as usize).div_ceil(64)],
            len,
            ones: 0,
            prefix: 0,
        }
    }

    pub(crate) fn from_bytes(len: u32, bytes: &[u8]) -> Self {
        let mut b = Self::new(len);
        for (byte_idx, &byte) in bytes.iter().enumerate() {
            for bit in 0..8 {
                if byte & (1 << bit) != 0
                    && let Ok(i) = u32::try_from(byte_idx * 8 + bit)
                {
                    b.set(i);
                }
            }
        }
        b
    }

    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut out: Vec<u8> = self.words.iter().flat_map(|w| w.to_le_bytes()).collect();
        out.truncate((self.len as usize).div_ceil(8));
        out
    }

    pub(crate) fn len(&self) -> u32 {
        self.len
    }

    pub(crate) fn get(&self, i: u32) -> bool {
        i < self.len
            && self
                .words
                .get((i / 64) as usize)
                .is_some_and(|w| w & (1 << (i % 64)) != 0)
    }

    /// Returns true when the bit was newly set.
    pub(crate) fn set(&mut self, i: u32) -> bool {
        if i >= self.len || self.get(i) {
            return false;
        }
        let Some(word) = self.words.get_mut((i / 64) as usize) else {
            return false;
        };
        *word |= 1 << (i % 64);
        self.ones += 1;
        if i == self.prefix {
            self.prefix = self.first_zero_from(i).unwrap_or(self.len);
        }
        true
    }

    pub(crate) fn count(&self) -> u32 {
        self.ones
    }

    /// Every index below this is set.
    pub(crate) fn prefix(&self) -> u32 {
        self.prefix
    }

    pub(crate) fn first_zero_from(&self, from: u32) -> Option<u32> {
        let from = from.max(self.prefix);
        let mut w = (from / 64) as usize;
        let mut mask = !0u64 << (from % 64);
        while let Some(&word) = self.words.get(w) {
            let free = !word & mask;
            if free != 0 {
                let i = u32::try_from(w * 64).ok()? + free.trailing_zeros();
                return (i < self.len).then_some(i);
            }
            w += 1;
            mask = !0;
        }
        None
    }

    pub(crate) fn is_full(&self) -> bool {
        self.ones == self.len
    }
}

fn window_chunks(chunk_size: u32) -> usize {
    usize::try_from(WINDOW_BYTES / u64::from(chunk_size.max(1)))
        .unwrap_or(MAX_WINDOW)
        .clamp(MIN_WINDOW, MAX_WINDOW)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum OutState {
    Offered,
    Sending,
    Stalled,
}

#[derive(Debug, Clone, Copy)]
struct Flight {
    sent_ms: u64,
    retries: u8,
}

pub(crate) struct Outgoing {
    pub peer: PeerId,
    /// The device of theirs that accepted it; the file goes there only.
    pub receiver: Option<DeviceId>,
    pub desc: FileDesc,
    pub kind: MediaKind,
    pub cipher: ChunkCipher,
    pub state: OutState,
    acked: Bitmap,
    in_flight: BTreeMap<u32, Flight>,
    /// Chunks already written to the sender's own sealed media copy.
    pub stored_copy: Bitmap,
}

pub(crate) enum AckOutcome {
    Ignored,
    Progress,
    Complete,
}

impl Outgoing {
    pub(crate) fn new(peer: PeerId, desc: FileDesc, kind: MediaKind) -> Self {
        let count = desc.chunk_count();
        let cipher = cipher_for(&desc);
        Self {
            peer,
            receiver: None,
            kind,
            cipher,
            state: OutState::Offered,
            acked: Bitmap::new(count),
            in_flight: BTreeMap::new(),
            stored_copy: Bitmap::new(count),
            desc,
        }
    }

    /// Peer accepted (or asked to resume) with a bitmap of chunks it holds.
    pub(crate) fn accept(&mut self, have: &[u8]) {
        let theirs = Bitmap::from_bytes(self.acked.len(), have);
        for i in 0..self.acked.len() {
            if theirs.get(i) {
                self.acked.set(i);
            }
        }
        self.in_flight.clear();
        self.state = OutState::Sending;
    }

    /// Connection lost: forget in-flight chunks until the session is back.
    pub(crate) fn pause(&mut self) {
        if self.state == OutState::Sending {
            self.in_flight.clear();
            self.state = OutState::Stalled;
        }
    }

    pub(crate) fn resume(&mut self) {
        if self.state == OutState::Stalled {
            self.state = OutState::Sending;
        }
    }

    /// Chunk indices the driver should read now, marking them in flight.
    pub(crate) fn schedule(&mut self, now_ms: u64) -> Vec<u32> {
        if self.state != OutState::Sending {
            return Vec::new();
        }
        let window = window_chunks(self.desc.chunk_size);
        let mut out = Vec::new();
        let mut i = 0;
        while self.in_flight.len() < window {
            let Some(next) = self.acked.first_zero_from(i) else {
                break;
            };
            i = next + 1;
            if self.in_flight.contains_key(&next) {
                continue;
            }
            self.in_flight.insert(
                next,
                Flight {
                    sent_ms: now_ms,
                    retries: 0,
                },
            );
            out.push(next);
        }
        out
    }

    pub(crate) fn on_ack(&mut self, next: u32, sack: u64) -> AckOutcome {
        if self.state != OutState::Sending || next > self.acked.len() {
            return AckOutcome::Ignored;
        }
        let mut advanced = false;
        for i in self.acked.prefix()..next {
            advanced |= self.acked.set(i);
            self.in_flight.remove(&i);
        }
        for bit in 0..64u32 {
            if sack & (1 << bit) != 0
                && let Some(i) = next.checked_add(1 + bit)
            {
                advanced |= self.acked.set(i);
                self.in_flight.remove(&i);
            }
        }
        if self.acked.is_full() {
            AckOutcome::Complete
        } else if advanced {
            AckOutcome::Progress
        } else {
            AckOutcome::Ignored
        }
    }

    /// Returns chunks to resend; flips to `Stalled` when retries run out.
    pub(crate) fn expired(&mut self, now_ms: u64) -> Vec<u32> {
        let mut resend = Vec::new();
        for (&i, f) in &mut self.in_flight {
            if now_ms.saturating_sub(f.sent_ms) < RTO_MS << f.retries.min(4) {
                continue;
            }
            if f.retries >= MAX_RETRIES {
                self.state = OutState::Stalled;
                break;
            }
            f.retries += 1;
            f.sent_ms = now_ms;
            resend.push(i);
        }
        if self.state == OutState::Stalled {
            self.in_flight.clear();
            return Vec::new();
        }
        resend
    }

    pub(crate) fn acked_bytes(&self) -> u64 {
        acked_bytes(&self.acked, &self.desc)
    }

    /// Where its chunks go, once a device accepted it.
    pub(crate) fn to(&self) -> Option<Addr> {
        self.receiver.map(|device| Addr::new(self.peer, device))
    }

    pub(crate) fn to_record(&self) -> TransferRecord {
        TransferRecord {
            outgoing: true,
            peer: self.peer,
            device: self.receiver,
            desc: self.desc.clone(),
            kind: self.kind.clone(),
            done: self.acked.to_bytes(),
            accepted: self.state != OutState::Offered,
        }
    }

    pub(crate) fn from_record(r: TransferRecord) -> Self {
        let mut t = Self::new(r.peer, r.desc, r.kind);
        t.acked = Bitmap::from_bytes(t.acked.len(), &r.done);
        t.stored_copy = t.acked.clone();
        if r.accepted {
            t.state = OutState::Stalled;
            t.receiver = Some(r.device.unwrap_or(DeviceId::FIRST));
        }
        t
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InState {
    Offered,
    Receiving,
}

pub(crate) struct Incoming {
    pub peer: PeerId,
    /// The device of theirs that offered it, and sends its chunks.
    pub device: DeviceId,
    pub desc: FileDesc,
    pub kind: MediaKind,
    pub cipher: ChunkCipher,
    pub state: InState,
    pub received: Bitmap,
    since_ack: u32,
}

pub(crate) enum ChunkOutcome {
    /// Verified: store `data` at `offset`; `ack` says whether to acknowledge now.
    Store {
        offset: u64,
        data: Vec<u8>,
        ack: bool,
        complete: bool,
    },
    Duplicate,
    Rejected,
}

impl Incoming {
    pub(crate) fn new(from: Addr, desc: FileDesc, kind: MediaKind) -> Self {
        let received = Bitmap::new(desc.chunk_count());
        Self {
            peer: from.peer,
            device: from.device,
            cipher: cipher_for(&desc),
            kind,
            state: InState::Offered,
            received,
            since_ack: 0,
            desc,
        }
    }

    pub(crate) fn sealed_at_rest(&self) -> bool {
        self.kind.is_media()
    }

    /// Bytes on disk: media keeps ciphertext (tag per chunk), files plaintext.
    pub(crate) fn stored_len(&self) -> u64 {
        stored_len(&self.desc, self.sealed_at_rest())
    }

    pub(crate) fn on_chunk(&mut self, index: u32, ciphertext: &[u8]) -> ChunkOutcome {
        if self.state != InState::Receiving || index >= self.received.len() {
            return ChunkOutcome::Rejected;
        }
        let expected = self.desc.chunk_len(index) as usize + CHUNK_TAG_LEN;
        if ciphertext.len() != expected {
            return ChunkOutcome::Rejected;
        }
        if self.received.get(index) {
            return ChunkOutcome::Duplicate;
        }
        let mut buf = ciphertext.to_vec();
        if self.cipher.open(index, &mut buf).is_err() {
            return ChunkOutcome::Rejected;
        }
        let sealed = self.sealed_at_rest();
        let data = if sealed { ciphertext.to_vec() } else { buf };
        self.received.set(index);
        self.since_ack += 1;
        let complete = self.received.is_full();
        let in_order = self.received.first_zero_from(0).is_none_or(|z| z > index);
        let ack = complete || self.since_ack >= ACK_EVERY || !in_order;
        if ack {
            self.since_ack = 0;
        }
        ChunkOutcome::Store {
            offset: chunk_offset(&self.desc, index, sealed),
            data,
            ack,
            complete,
        }
    }

    /// Cumulative ack plus a 64-chunk selective bitmap after it.
    pub(crate) fn ack_state(&self) -> (u32, u64) {
        let next = self
            .received
            .first_zero_from(0)
            .unwrap_or(self.received.len());
        let mut sack = 0u64;
        for bit in 0..64u32 {
            if let Some(i) = next.checked_add(1 + bit)
                && self.received.get(i)
            {
                sack |= 1 << bit;
            }
        }
        (next, sack)
    }

    pub(crate) fn received_bytes(&self) -> u64 {
        acked_bytes(&self.received, &self.desc)
    }

    /// The device it comes from.
    pub(crate) fn from(&self) -> Addr {
        Addr::new(self.peer, self.device)
    }

    pub(crate) fn to_record(&self) -> TransferRecord {
        TransferRecord {
            outgoing: false,
            peer: self.peer,
            device: Some(self.device),
            desc: self.desc.clone(),
            kind: self.kind.clone(),
            done: self.received.to_bytes(),
            accepted: self.state == InState::Receiving,
        }
    }

    pub(crate) fn from_record(r: TransferRecord) -> Self {
        let from = Addr::new(r.peer, r.device.unwrap_or(DeviceId::FIRST));
        let mut t = Self::new(from, r.desc, r.kind);
        t.received = Bitmap::from_bytes(t.received.len(), &r.done);
        if r.accepted {
            t.state = InState::Receiving;
        }
        t
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct TransferRecord {
    pub outgoing: bool,
    pub peer: PeerId,
    /// Incoming: the device it comes from; outgoing: the one that accepted.
    pub device: Option<DeviceId>,
    pub desc: FileDesc,
    pub kind: MediaKind,
    pub done: Vec<u8>,
    pub accepted: bool,
}

impl crate::Record for TransferRecord {
    const VERSION: u8 = 2;

    /// Version 1 went to or came from the contact's one device.
    fn upgrade(version: u8, body: &[u8]) -> Result<Self, crate::CoreError> {
        #[derive(Deserialize)]
        struct V1 {
            outgoing: bool,
            peer: PeerId,
            desc: FileDesc,
            kind: MediaKind,
            done: Vec<u8>,
            accepted: bool,
        }
        if version != 1 {
            return Err(crate::CoreError::Storage);
        }
        let v1: V1 = postcard::from_bytes(body).map_err(|_| crate::CoreError::Storage)?;
        let device = (!v1.outgoing || v1.accepted).then_some(DeviceId::FIRST);
        Ok(Self {
            outgoing: v1.outgoing,
            peer: v1.peer,
            device,
            desc: v1.desc,
            kind: v1.kind,
            done: v1.done,
            accepted: v1.accepted,
        })
    }
}

pub(crate) fn cipher_for(desc: &FileDesc) -> ChunkCipher {
    ChunkCipher::new(
        &FileKey::from_bytes(desc.key),
        desc.file_id,
        desc.chunk_count(),
    )
}

pub(crate) fn chunk_offset(desc: &FileDesc, index: u32, sealed: bool) -> u64 {
    let stride = u64::from(desc.chunk_size) + if sealed { CHUNK_TAG_LEN as u64 } else { 0 };
    u64::from(index) * stride
}

pub(crate) fn stored_len(desc: &FileDesc, sealed: bool) -> u64 {
    let tags = if sealed {
        u64::from(desc.chunk_count()) * CHUNK_TAG_LEN as u64
    } else {
        0
    };
    desc.size + tags
}

fn acked_bytes(map: &Bitmap, desc: &FileDesc) -> u64 {
    let last = map.len().saturating_sub(1);
    let full = u64::from(map.count()) * u64::from(desc.chunk_size);
    if map.get(last) {
        full - u64::from(desc.chunk_size - desc.chunk_len(last))
    } else {
        full
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_types::FileId;

    fn desc(size: u64, chunk_size: u32) -> FileDesc {
        FileDesc {
            file_id: FileId([1; 16]),
            name: "f".into(),
            mime: "application/octet-stream".into(),
            size,
            chunk_size,
            key: [9; 32],
            inline: None,
        }
    }

    fn seal(desc: &FileDesc, index: u32) -> Vec<u8> {
        let mut buf = vec![index.to_le_bytes()[0]; desc.chunk_len(index) as usize];
        cipher_for(desc).seal(index, &mut buf).unwrap();
        buf
    }

    #[test]
    fn bitmap_ops() {
        let mut b = Bitmap::new(70);
        assert!(b.set(0) && b.set(69) && !b.set(69) && !b.set(70));
        assert_eq!(b.count(), 2);
        assert_eq!(b.first_zero_from(0), Some(1));
        let back = Bitmap::from_bytes(70, &b.to_bytes());
        assert_eq!(back, b);
    }

    #[test]
    fn sender_window_ack_and_completion() {
        let d = desc(10 * 1024, 1024);
        let mut out = Outgoing::new(PeerId([2; 32]), d, MediaKind::File);
        assert!(out.schedule(0).is_empty());
        out.accept(&[0b0000_0011]);
        let first = out.schedule(0);
        assert_eq!(first, (2..10).collect::<Vec<_>>());
        assert!(matches!(out.on_ack(5, 0b1), AckOutcome::Progress));
        assert!(out.schedule(0).is_empty());
        assert!(matches!(out.on_ack(100, 0), AckOutcome::Ignored));
        assert!(matches!(out.on_ack(10, 0), AckOutcome::Complete));
    }

    #[test]
    fn sender_retransmits_then_stalls() {
        let mut out = Outgoing::new(PeerId([2; 32]), desc(4096, 1024), MediaKind::File);
        out.accept(&[]);
        out.schedule(0);
        assert!(out.expired(RTO_MS - 1).is_empty());
        assert_eq!(out.expired(RTO_MS).len(), 4);
        let mut t = RTO_MS;
        for _ in 0..10 {
            t += RTO_MS << 4;
            out.expired(t);
        }
        assert_eq!(out.state, OutState::Stalled);
        assert!(out.schedule(t).is_empty());
    }

    fn sender() -> Addr {
        Addr::new(PeerId([3; 32]), DeviceId(2))
    }

    fn receiving(d: &FileDesc) -> Incoming {
        let mut inc = Incoming::new(sender(), d.clone(), MediaKind::File);
        inc.state = InState::Receiving;
        inc
    }

    #[test]
    fn receiver_ignores_chunks_before_accepting() {
        let d = desc(2500, 1024);
        let mut inc = Incoming::new(sender(), d.clone(), MediaKind::File);
        assert!(matches!(
            inc.on_chunk(0, &seal(&d, 0)),
            ChunkOutcome::Rejected
        ));
    }

    #[test]
    fn receiver_validates_and_acks() {
        let d = desc(2500, 1024);
        let mut inc = receiving(&d);
        let c2 = seal(&d, 2);
        assert_eq!(c2.len(), 452 + CHUNK_TAG_LEN);
        match inc.on_chunk(2, &c2) {
            ChunkOutcome::Store {
                offset,
                ack,
                complete,
                data,
            } => {
                assert_eq!(
                    (offset, ack, complete, data.len()),
                    (2048, true, false, 452)
                );
            }
            _ => panic!("expected store"),
        }
        assert!(matches!(inc.on_chunk(2, &c2), ChunkOutcome::Duplicate));
        assert!(matches!(inc.on_chunk(1, &c2), ChunkOutcome::Rejected));
        assert!(matches!(inc.on_chunk(3, &c2), ChunkOutcome::Rejected));
        let mut forged = seal(&d, 0);
        forged[0] ^= 1;
        assert!(matches!(inc.on_chunk(0, &forged), ChunkOutcome::Rejected));
        assert_eq!(inc.ack_state(), (0, 0b10));
    }

    #[test]
    fn receiver_completes_out_of_order() {
        let d = desc(2500, 1024);
        let mut inc = receiving(&d);
        assert!(matches!(
            inc.on_chunk(2, &seal(&d, 2)),
            ChunkOutcome::Store { .. }
        ));
        assert!(matches!(
            inc.on_chunk(0, &seal(&d, 0)),
            ChunkOutcome::Store { .. }
        ));
        assert!(matches!(
            inc.on_chunk(1, &seal(&d, 1)),
            ChunkOutcome::Store {
                complete: true,
                ack: true,
                ..
            }
        ));
        assert_eq!(inc.ack_state(), (3, 0));
        assert_eq!(inc.received_bytes(), 2500);
    }

    #[test]
    fn media_is_stored_sealed_with_tag_stride() {
        let d = desc(100, 64);
        let mut inc = Incoming::new(
            sender(),
            d.clone(),
            MediaKind::Voice {
                duration_ms: 1,
                waveform: vec![],
            },
        );
        inc.state = InState::Receiving;
        assert_eq!(inc.stored_len(), 100 + 2 * CHUNK_TAG_LEN as u64);
        let ct = seal(&d, 1);
        match inc.on_chunk(1, &ct) {
            ChunkOutcome::Store { offset, data, .. } => {
                assert_eq!(offset, 64 + CHUNK_TAG_LEN as u64);
                assert_eq!(data, ct);
            }
            _ => panic!("expected store"),
        }
    }

    #[test]
    fn records_roundtrip() {
        let d = desc(4096, 1024);
        let mut out = Outgoing::new(PeerId([2; 32]), d.clone(), MediaKind::File);
        out.accept(&[0b1]);
        let restored = Outgoing::from_record(out.to_record());
        assert_eq!(restored.state, OutState::Stalled);
        assert_eq!(restored.acked_bytes(), 1024);

        let mut inc = Incoming::new(sender(), d, MediaKind::File);
        inc.state = InState::Receiving;
        let restored = Incoming::from_record(inc.to_record());
        assert_eq!(restored.state, InState::Receiving);
        assert_eq!(restored.from(), sender(), "the sending device is kept");
    }
}
