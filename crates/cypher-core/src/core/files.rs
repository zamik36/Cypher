use bytes::Bytes;
use cypher_crypto::FileKey;
use cypher_crypto::chunk::CHUNK_TAG_LEN;
use cypher_types::{Addr, DeviceId, FileId, MsgId, PeerId};
use rand_core::CryptoRngCore;

use super::Core;
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
    AckOutcome, ChunkOutcome, InState, Incoming, Outgoing, chunk_offset, cipher_for, stored_len,
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
        let content = Content::File {
            file_id: desc.file_id,
            name: desc.name.clone(),
            mime: desc.mime.clone(),
            size: desc.size,
            kind: kind.clone(),
        };
        self.store_message(StoredMessage {
            msg_id,
            peer,
            outgoing: true,
            sent_at_ms: self.now,
            status: MessageStatus::Pending,
            content: content.clone(),
        });
        self.sync_sent(peer, msg_id, content);
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
        sender: Addr,
        msg_id: MsgId,
        sent_at_ms: u64,
        desc: FileDesc,
        kind: MediaKind,
    ) {
        let (from, file_id) = (sender.peer, desc.file_id);
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
        let inc = Incoming::new(sender, desc, kind);
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
        let (from, len, sealed) = (inc.from(), inc.stored_len(), inc.sealed_at_rest());
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
        self.send_control_to(
            from,
            Body::FileCtl(FileCtl::Accept {
                file_id: *file_id,
                have: Vec::new(),
            }),
        );
    }

    /// One device of the contact accepted or refused a file. It goes to the
    /// first device that accepts; another that accepts later is told no.
    pub(super) fn on_file_ctl(&mut self, sender: Addr, ctl: FileCtl) {
        match ctl {
            FileCtl::Accept { file_id, have } => {
                let Some(out) = self
                    .outgoing
                    .get_mut(&file_id)
                    .filter(|o| o.peer == sender.peer)
                else {
                    // Finished, or taken by another device: the contact's
                    // device would wait for it forever.
                    if self.peers.contains_key(&sender.peer) {
                        let cancel = Body::FileCtl(FileCtl::Cancel { file_id });
                        self.send_control_to(sender, cancel);
                    }
                    return;
                };
                out.accept(sender.device, &have);
                let record = out.to_record();
                self.persist_transfer_record(file_id, &record);
                self.pump(&file_id);
            }
            FileCtl::Cancel { file_id } => {
                if self
                    .incoming
                    .get(&file_id)
                    .is_some_and(|i| i.from() == sender)
                {
                    self.drop_transfer(&file_id, Some(FailReason::Cancelled), false);
                } else {
                    self.stop_sending(file_id, sender);
                }
            }
        }
    }

    pub(super) fn cancel_transfer(&mut self, file_id: &FileId) {
        if self.tell_cancelled(file_id) {
            self.drop_transfer(file_id, Some(FailReason::Cancelled), false);
        }
    }

    /// One device of theirs refused a file, or stopped taking it. The file
    /// stays offered to the others: it ends when the last device receiving it
    /// stops and no other could still take it.
    fn stop_sending(&mut self, file_id: FileId, sender: Addr) {
        let Some(out) = self
            .outgoing
            .get_mut(&file_id)
            .filter(|o| o.peer == sender.peer)
        else {
            return;
        };
        out.receivers.remove(&sender.device);
        let idle = out.receivers.is_empty();
        let others = self
            .devices_of(sender.peer)
            .into_iter()
            .any(|d| d != sender.device);
        if idle && !others {
            self.drop_transfer(&file_id, Some(FailReason::Cancelled), false);
        }
    }

    /// Tells whoever takes part in a transfer that it ends: the device it
    /// comes from, or every device of theirs it was offered to.
    fn tell_cancelled(&mut self, file_id: &FileId) -> bool {
        let cancel = Body::FileCtl(FileCtl::Cancel { file_id: *file_id });
        if let Some(peer) = self.outgoing.get(file_id).map(|o| o.peer) {
            self.send_control(peer, cancel);
            true
        } else if let Some(from) = self.incoming.get(file_id).map(Incoming::from) {
            self.send_control_to(from, cancel);
            true
        } else {
            false
        }
    }

    /// Ends the transfers with one device of a contact.
    pub(super) fn cancel_transfers_with_device(&mut self, addr: Addr) {
        for out in self.outgoing.values_mut().filter(|o| o.peer == addr.peer) {
            out.receivers.remove(&addr.device);
        }
        let ids: Vec<FileId> = self
            .incoming
            .iter()
            .filter(|(_, i)| i.from() == addr)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.drop_transfer(&id, Some(FailReason::Cancelled), false);
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
        if self.outgoing.contains_key(file_id) && self.tell_cancelled(file_id) {
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
        let wanted = out.schedule(self.now);
        for (device, index) in wanted {
            self.request_chunk(file_id, device, index);
        }
    }

    /// Asks the driver for a chunk `device` is due; devices waiting for the
    /// same chunk share one read.
    fn request_chunk(&mut self, file_id: &FileId, device: DeviceId, index: u32) {
        let Some(out) = self.outgoing.get(file_id) else {
            return;
        };
        let waiting = self.reads.entry((*file_id, index)).or_default();
        let reading = !waiting.is_empty();
        if !waiting.contains(&device) {
            waiting.push(device);
        }
        if reading {
            return;
        }
        self.effects.push(Effect::ReadChunk {
            file_id: *file_id,
            index,
            offset: u64::from(index) * u64::from(out.desc.chunk_size),
            len: out.desc.chunk_len(index),
            headroom: CHUNK_HEADROOM,
        });
    }

    /// Seals the chunk in place inside the driver's read buffer and sends it
    /// as a complete frame to each device waiting for it: for one device the
    /// chunk data is never copied after the disk read.
    pub(super) fn on_chunk_read(&mut self, file_id: FileId, index: u32, mut buf: Vec<u8>) {
        let waiting = self.reads.remove(&(file_id, index)).unwrap_or_default();
        let Some(out) = self.outgoing.get_mut(&file_id) else {
            return;
        };
        let devices: Vec<DeviceId> = waiting.into_iter().filter(|d| out.sending_to(*d)).collect();
        if devices.is_empty() {
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
        let mut frames = Vec::with_capacity(devices.len());
        for (n, device) in devices.iter().enumerate() {
            let mut frame = if n + 1 == devices.len() {
                std::mem::take(&mut buf)
            } else {
                buf.clone()
            };
            let Some(headroom) = frame.first_chunk_mut::<CHUNK_HEADROOM>() else {
                return;
            };
            relay::write_chunk_headers(headroom, out.to(*device), &file_id, index);
            frames.push(Bytes::from(frame));
        }

        if out.kind.is_media()
            && out.stored_copy.set(index)
            && let Some(frame) = frames.first()
        {
            let offset = chunk_offset(&out.desc, index, true);
            self.effects.push(Effect::WriteChunk {
                file_id,
                offset,
                data: frame.slice(CHUNK_HEADROOM..),
            });
        }
        if self.is_ready() {
            self.effects
                .extend(frames.into_iter().map(Effect::Transmit));
        }
    }

    pub(super) fn on_chunk(&mut self, sender: Addr, file_id: FileId, index: u32, data: &[u8]) {
        let Some(inc) = self
            .incoming
            .get_mut(&file_id)
            .filter(|i| i.from() == sender)
        else {
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
        let frame = relay::ack_frame(inc.from(), file_id, next, sack, &tag);
        if self.is_ready() {
            self.effects.push(Effect::Transmit(frame));
        }
    }

    pub(super) fn on_file_ack(
        &mut self,
        sender: Addr,
        file_id: FileId,
        next: u32,
        sack: u64,
        tag: &[u8; 16],
    ) {
        let Some(out) = self
            .outgoing
            .get_mut(&file_id)
            .filter(|o| o.peer == sender.peer)
        else {
            return;
        };
        if !out.cipher.verify_ack(next, sack, tag) {
            return;
        }
        match out.on_ack(sender.device, next, sack) {
            AckOutcome::Ignored => {}
            AckOutcome::Progress => {
                let (bytes, total) = (out.acked_bytes(), out.desc.size);
                self.report_progress(file_id, bytes, total);
                self.pump(&file_id);
            }
            // That device has it all; the transfer is done once every device
            // that took it has.
            AckOutcome::Complete => {
                out.receivers.remove(&sender.device);
                if out.receivers.is_empty() {
                    let copy_complete = !out.kind.is_media() || out.stored_copy.is_full();
                    self.drop_transfer(&file_id, None, copy_complete);
                } else {
                    let record = out.to_record();
                    self.persist_transfer_record(file_id, &record);
                }
            }
        }
    }

    pub(super) fn retransmit_expired(&mut self) {
        let now = self.now;
        let mut resend = Vec::new();
        for (id, out) in &mut self.outgoing {
            resend.extend(out.expired(now).into_iter().map(|(d, i)| (*id, d, i)));
        }
        for (id, device, index) in resend {
            self.request_chunk(&id, device, index);
        }
    }

    pub(super) fn pause_transfers(&mut self) {
        for out in self.outgoing.values_mut() {
            out.pause();
        }
        // Reads still on their way come back to nobody.
        self.reads.clear();
    }

    pub(super) fn resume_transfers(&mut self) {
        let resumable: Vec<FileId> = self
            .outgoing
            .iter_mut()
            .filter_map(|(id, o)| o.resume().then_some(*id))
            .collect();
        for id in resumable {
            self.pump(&id);
        }
        let receiving: Vec<(FileId, Addr, Vec<u8>)> = self
            .incoming
            .iter()
            .filter(|(_, i)| i.state == InState::Receiving)
            .map(|(id, i)| (*id, i.from(), i.received.to_bytes()))
            .collect();
        for (file_id, from, have) in receiving {
            self.send_control_to(from, Body::FileCtl(FileCtl::Accept { file_id, have }));
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
