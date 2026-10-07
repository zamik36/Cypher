use bytes::Bytes;
use cypher_crypto::FileKey;
use cypher_crypto::chunk::CHUNK_TAG_LEN;
use cypher_types::{FileId, MsgId, PeerId};
use rand_core::CryptoRngCore;

use super::{Core, contact};
use crate::api::{Content, Effect, Event, FailReason, MediaKind, MessageStatus, StoredMessage};
use crate::envelope::{
    Body, FILE_CHUNK_SIZE, FileCtl, FileDesc, MAX_FILE_SIZE, MAX_INLINE_LEN, MAX_MIME_LEN,
    MEDIA_CHUNK_SIZE,
};
use crate::fs_name;
use crate::media::MediaKey;
use crate::relay::{self, CHUNK_HEADROOM};
use crate::store::{StoreOp, Table};
use crate::transfer::{
    AckOutcome, ChunkOutcome, InState, Incoming, OutState, Outgoing, chunk_offset, cipher_for,
    stored_len,
};

const PROGRESS_INTERVAL_MS: u64 = 100;
const PERSIST_EVERY_CHUNKS: u32 = 32;

/// A file or media message the user is sending.
pub(super) struct NewFile {
    pub peer: PeerId,
    pub msg_id: MsgId,
    pub file_id: FileId,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub kind: MediaKind,
    pub inline: Option<Vec<u8>>,
}

impl<R: CryptoRngCore> Core<R> {
    pub(super) fn send_file(&mut self, file: NewFile) {
        if !self.accepts(&file) {
            self.emit(Event::MessageStatus {
                msg_id: file.msg_id,
                status: MessageStatus::Failed,
            });
            return;
        }
        let (peer, msg_id, kind) = (file.peer, file.msg_id, file.kind.clone());
        let desc = self.describe(file);
        self.store_message(StoredMessage {
            msg_id,
            peer,
            outgoing: true,
            sent_at_ms: self.now,
            status: MessageStatus::Pending,
            content: Content::File {
                file_id: desc.file_id,
                name: desc.name.clone(),
                mime: desc.mime.clone(),
                size: desc.size,
                kind: kind.clone(),
            },
        });
        if let Some(data) = &desc.inline {
            self.store_inline(&desc, data);
        } else {
            self.start_outgoing(peer, &desc, &kind);
        }
        self.enqueue(peer, msg_id, Body::File { desc, kind }, true);
    }

    fn accepts(&self, file: &NewFile) -> bool {
        let inline_ok = file.inline.as_ref().is_none_or(|d| {
            file.kind.is_media() && d.len() <= MAX_INLINE_LEN && d.len() as u64 == file.size
        });
        self.peers.contains_key(&file.peer)
            && file.size <= MAX_FILE_SIZE
            && file.mime.len() <= MAX_MIME_LEN
            && inline_ok
            && !self.outgoing.contains_key(&file.file_id)
    }

    /// Descriptor of an accepted file with a fresh key and a sanitized name.
    fn describe(&mut self, file: NewFile) -> FileDesc {
        let chunk_size = match &file.inline {
            Some(data) => u32::try_from(data.len()).unwrap_or(1).max(1),
            None if file.kind.is_media() => MEDIA_CHUNK_SIZE,
            None => FILE_CHUNK_SIZE,
        };
        FileDesc {
            file_id: file.file_id,
            name: fs_name::sanitize(&file.name),
            mime: file.mime,
            size: file.size,
            chunk_size,
            key: *FileKey::random(&mut self.rng).as_bytes(),
            inline: file.inline,
        }
    }

    /// Registers a chunked transfer; media also opens the sender's sealed copy.
    fn start_outgoing(&mut self, peer: PeerId, desc: &FileDesc, kind: &MediaKind) {
        if kind.is_media() {
            self.persist_media_key(desc);
            self.effects.push(Effect::OpenSink {
                file_id: desc.file_id,
                len: stored_len(desc, true),
                sealed: true,
            });
        }
        let out = Outgoing::new(peer, desc.clone(), kind.clone());
        self.persist_transfer_record(desc.file_id, &out.to_record());
        self.outgoing.insert(desc.file_id, out);
    }

    /// Small media travels inside the message; both sides keep a sealed
    /// single-chunk copy so playback always goes through the same path.
    fn store_inline(&mut self, desc: &FileDesc, data: &[u8]) {
        let mut sealed = data.to_vec();
        if cipher_for(desc).seal(0, &mut sealed).is_err() {
            return;
        }
        let file_id = desc.file_id;
        self.persist_media_key(desc);
        self.effects.push(Effect::OpenSink {
            file_id,
            len: sealed.len() as u64,
            sealed: true,
        });
        self.effects.push(Effect::WriteChunk {
            file_id,
            offset: 0,
            data: Bytes::from(sealed),
        });
        self.effects.push(Effect::CloseSink {
            file_id,
            complete: true,
        });
        self.emit(Event::TransferComplete { file_id });
    }

    pub(super) fn on_file_offer(
        &mut self,
        from: PeerId,
        msg_id: MsgId,
        sent_at_ms: u64,
        desc: FileDesc,
        kind: MediaKind,
    ) {
        let file_id = desc.file_id;
        if self.incoming.contains_key(&file_id) || self.outgoing.contains_key(&file_id) {
            return;
        }
        let name = fs_name::sanitize(&desc.name);
        self.store_message(StoredMessage {
            msg_id,
            peer: from,
            outgoing: false,
            sent_at_ms,
            status: MessageStatus::Delivered,
            content: Content::File {
                file_id,
                name: name.clone(),
                mime: desc.mime.clone(),
                size: desc.size,
                kind: kind.clone(),
            },
        });

        if let Some(data) = &desc.inline {
            if kind.is_media() {
                self.store_inline(&desc, data);
            }
            return;
        }

        let media = kind.is_media();
        let (size, mime) = (desc.size, desc.mime.clone());
        let inc = Incoming::new(from, desc, kind);
        self.persist_transfer_record(file_id, &inc.to_record());
        self.incoming.insert(file_id, inc);
        if media {
            self.accept_file(&file_id);
        } else {
            self.emit(Event::TransferOffered {
                peer: from,
                file_id,
                name,
                size,
                mime,
            });
        }
    }

    /// Offers still waiting for an answer, told again after a restart: the
    /// record survives, but whoever shows them to the user does not.
    pub(super) fn announce_pending_offers(&mut self) {
        let mut pending: Vec<FileId> = self
            .incoming
            .iter()
            .filter(|(_, inc)| inc.state == InState::Offered && !inc.kind.is_media())
            .map(|(file_id, _)| *file_id)
            .collect();
        pending.sort_unstable_by_key(|id| *id.as_bytes());
        for file_id in pending {
            if let Some(inc) = self.incoming.get(&file_id) {
                let offer = Event::TransferOffered {
                    peer: inc.peer,
                    file_id,
                    name: fs_name::sanitize(&inc.desc.name),
                    size: inc.desc.size,
                    mime: inc.desc.mime.clone(),
                };
                self.emit(offer);
            }
        }
    }

    pub(super) fn accept_file(&mut self, file_id: &FileId) {
        let Some(inc) = self.incoming.get_mut(file_id) else {
            return;
        };
        if inc.state != InState::Offered {
            return;
        }
        inc.state = InState::Receiving;
        let (peer, len, sealed) = (inc.peer, inc.stored_len(), inc.sealed_at_rest());
        if sealed {
            let key = MediaKey::from_desc(&inc.desc);
            self.persist_media(&key);
        }
        self.effects.push(Effect::OpenSink {
            file_id: *file_id,
            len,
            sealed,
        });
        self.persist_incoming(file_id);
        self.send_control(
            peer,
            Body::FileCtl(FileCtl::Accept {
                file_id: *file_id,
                have: Vec::new(),
            }),
        );
    }

    pub(super) fn on_file_ctl(&mut self, from: PeerId, ctl: FileCtl) {
        match ctl {
            FileCtl::Accept { file_id, have } => {
                let Some(out) = self.outgoing.get_mut(&file_id).filter(|o| o.peer == from) else {
                    return;
                };
                out.accept(&have);
                let record = out.to_record();
                self.persist_transfer_record(file_id, &record);
                self.pump(&file_id);
            }
            FileCtl::Cancel { file_id } => {
                let ours = self.outgoing.get(&file_id).is_some_and(|o| o.peer == from)
                    || self.incoming.get(&file_id).is_some_and(|i| i.peer == from);
                if ours {
                    self.drop_transfer(&file_id, Some(FailReason::Cancelled), false);
                }
            }
        }
    }

    pub(super) fn cancel_transfer(&mut self, file_id: &FileId) {
        let peer = self
            .outgoing
            .get(file_id)
            .map(|o| o.peer)
            .or_else(|| self.incoming.get(file_id).map(|i| i.peer));
        if let Some(peer) = peer {
            self.send_control(peer, Body::FileCtl(FileCtl::Cancel { file_id: *file_id }));
            self.drop_transfer(file_id, Some(FailReason::Cancelled), false);
        }
    }

    pub(super) fn cancel_transfers_with(&mut self, peer: &PeerId) {
        let ids: Vec<FileId> = self
            .outgoing
            .iter()
            .filter(|(_, o)| o.peer == *peer)
            .map(|(id, _)| *id)
            .chain(
                self.incoming
                    .iter()
                    .filter(|(_, i)| i.peer == *peer)
                    .map(|(id, _)| *id),
            )
            .collect();
        for id in ids {
            self.drop_transfer(&id, Some(FailReason::Cancelled), false);
        }
    }

    pub(super) fn fail_outgoing(&mut self, file_id: &FileId, reason: FailReason) {
        if let Some(peer) = self.outgoing.get(file_id).map(|o| o.peer) {
            self.send_control(peer, Body::FileCtl(FileCtl::Cancel { file_id: *file_id }));
            self.drop_transfer(file_id, Some(reason), false);
        }
    }

    /// Forgets a transfer; `reason == None` means it completed.
    fn drop_transfer(&mut self, file_id: &FileId, reason: Option<FailReason>, complete: bool) {
        let (had_sink, media) = match (self.outgoing.remove(file_id), self.incoming.remove(file_id))
        {
            (Some(out), _) => (out.kind.is_media(), out.kind.is_media()),
            (None, Some(inc)) => (inc.state == InState::Receiving, inc.sealed_at_rest()),
            (None, None) => return,
        };
        if had_sink {
            self.effects.push(Effect::CloseSink {
                file_id: *file_id,
                complete,
            });
        }
        if media && !complete {
            self.persist(StoreOp::Delete {
                table: Table::Media,
                key: file_id.to_vec(),
            });
        }
        self.progress_at.remove(file_id);
        self.persist(StoreOp::Delete {
            table: Table::Transfers,
            key: file_id.to_vec(),
        });
        self.emit(match reason {
            Some(reason) => Event::TransferFailed {
                file_id: *file_id,
                reason,
            },
            None => Event::TransferComplete { file_id: *file_id },
        });
    }

    fn pump(&mut self, file_id: &FileId) {
        if !self.is_ready() {
            return;
        }
        let Some(out) = self.outgoing.get_mut(file_id) else {
            return;
        };
        let indices = out.schedule(self.now);
        for index in indices {
            self.request_chunk(file_id, index);
        }
    }

    fn request_chunk(&mut self, file_id: &FileId, index: u32) {
        let Some(out) = self.outgoing.get(file_id) else {
            return;
        };
        self.effects.push(Effect::ReadChunk {
            file_id: *file_id,
            index,
            offset: u64::from(index) * u64::from(out.desc.chunk_size),
            len: out.desc.chunk_len(index),
            headroom: CHUNK_HEADROOM,
        });
    }

    /// Seals the chunk in place inside the driver's read buffer and sends it
    /// as a complete frame: the chunk data is never copied after the disk read.
    pub(super) fn on_chunk_read(&mut self, file_id: FileId, index: u32, mut buf: Vec<u8>) {
        let Some(out) = self.outgoing.get_mut(&file_id) else {
            return;
        };
        if out.state != OutState::Sending {
            return;
        }
        let expected = CHUNK_HEADROOM + out.desc.chunk_len(index) as usize;
        if buf.len() != expected {
            self.fail_outgoing(&file_id, FailReason::SourceUnavailable);
            return;
        }
        let (_, payload) = buf.split_at_mut(CHUNK_HEADROOM);
        let Ok(tag) = out.cipher.seal_detached(index, payload) else {
            return;
        };
        buf.reserve_exact(CHUNK_TAG_LEN);
        buf.extend_from_slice(&tag);
        let Some(headroom) = buf.first_chunk_mut::<CHUNK_HEADROOM>() else {
            return;
        };
        relay::write_chunk_headers(headroom, contact(out.peer), &file_id, index);
        let frame = Bytes::from(buf);

        if out.kind.is_media() && out.stored_copy.set(index) {
            let offset = chunk_offset(&out.desc, index, true);
            self.effects.push(Effect::WriteChunk {
                file_id,
                offset,
                data: frame.slice(CHUNK_HEADROOM..),
            });
        }
        if self.is_ready() {
            self.effects.push(Effect::Transmit(frame));
        }
    }

    pub(super) fn on_chunk(&mut self, from: PeerId, file_id: FileId, index: u32, data: &[u8]) {
        let Some(inc) = self.incoming.get_mut(&file_id).filter(|i| i.peer == from) else {
            return;
        };
        match inc.on_chunk(index, data) {
            ChunkOutcome::Store {
                offset,
                data,
                ack,
                complete,
            } => {
                let received = inc.received.count();
                let (bytes, total) = (inc.received_bytes(), inc.desc.size);
                self.effects.push(Effect::WriteChunk {
                    file_id,
                    offset,
                    data: Bytes::from(data),
                });
                if ack {
                    self.send_file_ack(&file_id);
                }
                if complete {
                    self.drop_transfer(&file_id, None, true);
                    return;
                }
                if received % PERSIST_EVERY_CHUNKS == 0 {
                    self.persist_incoming(&file_id);
                }
                self.report_progress(file_id, bytes, total);
            }
            ChunkOutcome::Duplicate => self.send_file_ack(&file_id),
            ChunkOutcome::Rejected => {}
        }
    }

    fn send_file_ack(&mut self, file_id: &FileId) {
        let Some(inc) = self.incoming.get(file_id) else {
            return;
        };
        let (next, sack) = inc.ack_state();
        let tag = inc.cipher.ack_tag(next, sack);
        let frame = relay::ack_frame(contact(inc.peer), file_id, next, sack, &tag);
        if self.is_ready() {
            self.effects.push(Effect::Transmit(frame));
        }
    }

    pub(super) fn on_file_ack(
        &mut self,
        from: PeerId,
        file_id: FileId,
        next: u32,
        sack: u64,
        tag: &[u8; 16],
    ) {
        let Some(out) = self.outgoing.get_mut(&file_id).filter(|o| o.peer == from) else {
            return;
        };
        if !out.cipher.verify_ack(next, sack, tag) {
            return;
        }
        match out.on_ack(next, sack) {
            AckOutcome::Ignored => {}
            AckOutcome::Progress => {
                let (bytes, total) = (out.acked_bytes(), out.desc.size);
                self.report_progress(file_id, bytes, total);
                self.pump(&file_id);
            }
            AckOutcome::Complete => {
                let copy_complete = !out.kind.is_media() || out.stored_copy.is_full();
                self.drop_transfer(&file_id, None, copy_complete);
            }
        }
    }

    pub(super) fn retransmit_expired(&mut self) {
        let now = self.now;
        let mut resend = Vec::new();
        for (id, out) in &mut self.outgoing {
            if out.state == OutState::Sending {
                resend.extend(out.expired(now).into_iter().map(|i| (*id, i)));
            }
        }
        for (id, index) in resend {
            self.request_chunk(&id, index);
        }
    }

    pub(super) fn pause_transfers(&mut self) {
        for out in self.outgoing.values_mut() {
            out.pause();
        }
    }

    pub(super) fn resume_transfers(&mut self) {
        let resumable: Vec<FileId> = self
            .outgoing
            .iter_mut()
            .filter(|(_, o)| o.state == OutState::Stalled)
            .map(|(id, o)| {
                o.resume();
                *id
            })
            .collect();
        for id in resumable {
            self.pump(&id);
        }
        let receiving: Vec<(FileId, PeerId, Vec<u8>)> = self
            .incoming
            .iter()
            .filter(|(_, i)| i.state == InState::Receiving)
            .map(|(id, i)| (*id, i.peer, i.received.to_bytes()))
            .collect();
        for (file_id, peer, have) in receiving {
            self.send_control(peer, Body::FileCtl(FileCtl::Accept { file_id, have }));
        }
    }

    fn report_progress(&mut self, file_id: FileId, bytes: u64, total: u64) {
        let last = self.progress_at.get(&file_id).copied().unwrap_or(0);
        if self.now.saturating_sub(last) < PROGRESS_INTERVAL_MS {
            return;
        }
        self.progress_at.insert(file_id, self.now);
        self.emit(Event::TransferProgress {
            file_id,
            bytes,
            total,
        });
    }

    fn persist_incoming(&mut self, file_id: &FileId) {
        if let Some(inc) = self.incoming.get(file_id) {
            let record = inc.to_record();
            self.persist_transfer_record(*file_id, &record);
        }
    }

    fn persist_media_key(&mut self, desc: &FileDesc) {
        self.persist_media(&MediaKey::from_desc(desc));
    }

    fn persist_media(&mut self, key: &MediaKey) {
        let op = self
            .vault
            .put(Table::Media, key.file_id.to_vec(), key, &mut self.rng);
        self.persist(op);
    }

    fn persist_transfer_record(
        &mut self,
        file_id: FileId,
        record: &crate::transfer::TransferRecord,
    ) {
        let op = self
            .vault
            .put(Table::Transfers, file_id.to_vec(), record, &mut self.rng);
        self.persist(op);
    }
}
