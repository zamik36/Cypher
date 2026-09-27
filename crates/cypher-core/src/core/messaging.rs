use bytes::Bytes;
use cypher_crypto::{Header, InitHeader, PrekeyBundle, handshake, sealed};
use cypher_types::{MsgId, PeerId};
use cypher_wire::{ClientMsg, DeliveryStatus};
use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use super::anon::Readiness;
use super::{Core, Pending};
use crate::CoreError;
use crate::api::{Content, Event, FailReason, MessageStatus, StoredMessage};
use crate::envelope::{Body, Envelope, MAX_RECEIPT_IDS, MAX_TEXT_LEN, ReceiptKind};
use crate::peer::Peer;
use crate::relay::{self, RelayBody};
use crate::store::{StoreOp, Table, message_key};

const RETRY_OFFLINE_MS: u64 = 30_000;
const RETRY_BUSY_MS: u64 = 2_000;
const RETRY_FAILED_MS: u64 = 5_000;

/// An end-to-end message waiting for server acceptance. Stored as padded
/// plaintext and encrypted at send time, so a session reset never strands it.
#[derive(Serialize, Deserialize)]
pub(crate) struct OutboxItem {
    pub msg_id: MsgId,
    pub peer: PeerId,
    envelope: Vec<u8>,
    /// User-visible message whose status the UI tracks.
    tracked: bool,
    #[serde(skip)]
    in_flight: bool,
    #[serde(skip)]
    next_try: u64,
}

impl Drop for OutboxItem {
    fn drop(&mut self) {
        self.envelope.zeroize();
    }
}

impl<R: CryptoRngCore> Core<R> {
    pub(super) fn send_text(
        &mut self,
        peer: PeerId,
        msg_id: MsgId,
        text: String,
        reply_to: Option<MsgId>,
    ) {
        if text.is_empty() || text.len() > MAX_TEXT_LEN || !self.peers.contains_key(&peer) {
            self.emit(Event::MessageStatus {
                msg_id,
                status: MessageStatus::Failed,
            });
            return;
        }
        let stored = StoredMessage {
            msg_id,
            peer,
            outgoing: true,
            sent_at_ms: self.now,
            status: MessageStatus::Pending,
            content: Content::Text {
                text: text.clone(),
                reply_to,
            },
        };
        self.store_message(stored);
        self.enqueue(peer, msg_id, Body::Text { text, reply_to }, true);
    }

    pub(super) fn mark_read(&mut self, peer: PeerId, ids: &[MsgId]) {
        for chunk in ids.chunks(MAX_RECEIPT_IDS) {
            self.send_control(
                peer,
                Body::Receipt {
                    kind: ReceiptKind::Read,
                    ids: chunk.to_vec(),
                },
            );
        }
    }

    pub(super) fn remove_peer(&mut self, peer: &PeerId) {
        if self.peers.remove(peer).is_none() {
            return;
        }
        self.persist(StoreOp::Delete {
            table: Table::Peers,
            key: peer.to_vec(),
        });
        let stale: Vec<MsgId> = self
            .outbox
            .values()
            .filter(|i| i.peer == *peer)
            .map(|i| i.msg_id)
            .collect();
        for id in stale {
            self.drop_outbox(&id);
        }
        self.cancel_transfers_with(peer);
    }

    /// Sends a protocol message that has no UI representation.
    pub(super) fn send_control(&mut self, peer: PeerId, body: Body) {
        let mut id = [0u8; 16];
        self.rng.fill_bytes(&mut id);
        self.enqueue(peer, MsgId(id), body, false);
    }

    pub(super) fn enqueue(&mut self, peer: PeerId, msg_id: MsgId, body: Body, tracked: bool) {
        let envelope = Envelope {
            msg_id,
            sent_at_ms: self.now,
            body,
        }
        .encode();
        let item = OutboxItem {
            msg_id,
            peer,
            envelope,
            tracked,
            in_flight: false,
            next_try: 0,
        };
        let op = self
            .vault
            .put(Table::Outbox, msg_id.to_vec(), &item, &mut self.rng);
        self.persist(op);
        self.outbox.insert(msg_id, item);
        self.send_item(msg_id);
    }

    pub(super) fn flush_outbox(&mut self) {
        if !self.is_ready() {
            return;
        }
        let due: Vec<MsgId> = self
            .outbox
            .values()
            .filter(|i| !i.in_flight && i.next_try <= self.now)
            .map(|i| i.msg_id)
            .collect();
        for id in due {
            self.send_item(id);
        }
    }

    /// Encrypts and sends one outbox item. The advanced ratchet is persisted
    /// before the ciphertext is handed to the transport, so a crash can never
    /// lead to message-key reuse.
    fn send_item(&mut self, msg_id: MsgId) {
        if !self.is_ready() {
            return;
        }
        let Some(item) = self.outbox.get_mut(&msg_id) else {
            return;
        };
        let Some(peer) = self.peers.get_mut(&item.peer) else {
            return;
        };
        if item.in_flight || !peer.ratchet.can_send() {
            return;
        }
        let Ok((header, ct)) = peer.ratchet.encrypt(&item.envelope, b"") else {
            return;
        };
        let body = Bytes::from(relay::message_body(
            peer.pending_init.as_ref(),
            &header,
            &ct,
        ));
        item.in_flight = true;
        let peer_id = item.peer;

        self.persist_peer(&peer_id);
        let req_id = self.alloc_req();
        self.pending.insert(
            req_id,
            (
                Pending::Send {
                    msg_id,
                    peer: peer_id,
                    body: body.clone(),
                },
                self.now + super::REQUEST_TIMEOUT_MS,
            ),
        );
        self.transmit(
            req_id,
            ClientMsg::Send {
                to: peer_id,
                want_ack: true,
                body,
            },
        );
    }

    pub(super) fn on_send_ack(
        &mut self,
        msg_id: MsgId,
        peer: PeerId,
        body: &Bytes,
        status: DeliveryStatus,
    ) {
        match status {
            DeliveryStatus::Delivered => self.complete_outbox(msg_id, MessageStatus::Sent),
            DeliveryStatus::Offline => match self
                .peers
                .get(&peer)
                .and_then(|p| p.inbox.map(|i| (i, p.identity_dh)))
            {
                Some((inbox, identity_dh)) => self.put_inbox(msg_id, inbox, &identity_dh, body),
                None => self.retry_later(msg_id, RETRY_OFFLINE_MS),
            },
            DeliveryStatus::Busy => self.retry_later(msg_id, RETRY_BUSY_MS),
        }
    }

    pub(super) fn on_send_failed(&mut self, msg_id: MsgId) {
        self.retry_later(msg_id, RETRY_FAILED_MS);
    }

    pub(super) fn on_inbox_queued(&mut self, msg_id: MsgId) {
        self.complete_outbox(msg_id, MessageStatus::Queued);
    }

    fn put_inbox(&mut self, msg_id: MsgId, inbox: [u8; 32], identity_dh: &[u8; 32], body: &[u8]) {
        match self.anon.readiness(self.now) {
            Readiness::Onion | Readiness::Session => {}
            Readiness::Wait => return self.retry_later(msg_id, RETRY_BUSY_MS),
            Readiness::Unavailable => return self.retry_later(msg_id, RETRY_OFFLINE_MS),
        }
        let mut plain = Zeroizing::new(Vec::with_capacity(32 + body.len()));
        plain.extend_from_slice(self.peer_id.as_bytes());
        plain.extend_from_slice(body);
        match sealed::seal(identity_dh, &plain, &mut self.rng) {
            Ok(item) => self.request(
                ClientMsg::InboxPut {
                    inbox,
                    item: Bytes::from(item),
                },
                Pending::InboxPut { msg_id },
                true,
            ),
            Err(_) => self.retry_later(msg_id, RETRY_OFFLINE_MS),
        }
    }

    fn retry_later(&mut self, msg_id: MsgId, delay: u64) {
        if let Some(item) = self.outbox.get_mut(&msg_id) {
            item.in_flight = false;
            item.next_try = self.now + delay;
        }
    }

    fn complete_outbox(&mut self, msg_id: MsgId, status: MessageStatus) {
        let tracked = self.outbox.get(&msg_id).is_some_and(|i| i.tracked);
        self.drop_outbox(&msg_id);
        if tracked {
            self.set_status(msg_id, status);
        }
    }

    fn drop_outbox(&mut self, msg_id: &MsgId) {
        if self.outbox.remove(msg_id).is_some() {
            self.persist(StoreOp::Delete {
                table: Table::Outbox,
                key: msg_id.to_vec(),
            });
        }
    }

    pub(super) fn store_message(&mut self, msg: StoredMessage) {
        let key = message_key(&msg.peer, msg.sent_at_ms, &msg.msg_id);
        let op = self.vault.put(Table::Messages, key, &msg, &mut self.rng);
        self.persist(op);
        self.emit(Event::Message(msg));
    }

    pub(super) fn set_status(&mut self, msg_id: MsgId, status: MessageStatus) {
        let op = self.vault.put(
            Table::MessageStatus,
            msg_id.to_vec(),
            &status,
            &mut self.rng,
        );
        self.persist(op);
        self.emit(Event::MessageStatus { msg_id, status });
    }

    pub(super) fn on_keys(
        &mut self,
        link: String,
        peer: PeerId,
        base: &[u8],
        opk: Option<(u32, [u8; 32])>,
    ) {
        let mut raw = Vec::with_capacity(PrekeyBundle::MAX_LEN);
        raw.extend_from_slice(base);
        match opk {
            Some((id, key)) => {
                raw.push(1);
                raw.extend_from_slice(&id.to_le_bytes());
                raw.extend_from_slice(&key);
            }
            None => raw.push(0),
        }
        let initiated = PrekeyBundle::decode(&raw)
            .ok()
            .filter(|b| b.identity == peer)
            .and_then(|b| {
                handshake::initiate(&self.identity, &b, &mut self.rng)
                    .ok()
                    .map(|(r, init)| (r, init, b.identity_dh))
            });
        let Some((ratchet, init, identity_dh)) = initiated else {
            self.emit(Event::JoinFailed {
                link,
                reason: FailReason::InvalidKeys,
            });
            return;
        };

        if let Some(existing) = self.peers.get(&peer)
            && !existing.is_unconfirmed_initiator()
        {
            self.emit(Event::PeerAdded {
                peer,
                initiated_by_us: true,
            });
            return;
        }
        let inbox = self.peers.get(&peer).and_then(|p| p.inbox);
        self.peers.insert(
            peer,
            Peer {
                ratchet,
                pending_init: Some(init),
                accepted_ephemerals: Vec::new(),
                identity_dh,
                inbox,
                hello_sent: false,
            },
        );
        self.requeue_for(&peer);
        self.emit(Event::PeerAdded {
            peer,
            initiated_by_us: true,
        });
        self.send_hello(peer);
    }

    fn send_hello(&mut self, peer: PeerId) {
        let Some(p) = self.peers.get_mut(&peer) else {
            return;
        };
        if p.hello_sent {
            return;
        }
        p.hello_sent = true;
        self.persist_peer(&peer);
        let inbox = self.inbox_id;
        self.send_control(peer, Body::Hello { inbox });
    }

    /// Makes in-flight messages for `peer` eligible for re-encryption on a
    /// freshly established session.
    fn requeue_for(&mut self, peer: &PeerId) {
        for item in self.outbox.values_mut().filter(|i| i.peer == *peer) {
            item.in_flight = false;
            item.next_try = 0;
        }
    }

    pub(super) fn on_relay(&mut self, from: PeerId, body: &Bytes, via_inbox: bool) {
        match RelayBody::decode(body) {
            Ok(RelayBody::Message {
                init,
                header,
                ciphertext,
            }) => self.on_message(from, init.as_ref(), &header, &ciphertext),
            Ok(RelayBody::Chunk {
                file_id,
                index,
                data,
            }) if !via_inbox => {
                self.on_chunk(from, file_id, index, &data);
            }
            Ok(RelayBody::Ack {
                file_id,
                next,
                sack,
                tag,
            }) if !via_inbox => self.on_file_ack(from, file_id, next, sack, &tag),
            _ => {}
        }
    }

    fn on_message(&mut self, from: PeerId, init: Option<&InitHeader>, header: &Header, ct: &[u8]) {
        let result = match init {
            Some(init) => self.accept_init(from, init, header, ct),
            None => self.decrypt_existing(from, header, ct),
        };
        let plaintext = match result {
            Ok(pt) => Zeroizing::new(pt),
            Err(CoreError::Conflict) => return,
            Err(_) => {
                self.emit(Event::Warning {
                    reason: FailReason::DecryptFailed,
                });
                return;
            }
        };
        self.persist_peer(&from);
        let Ok(env) = Envelope::decode(&plaintext) else {
            return;
        };
        if !self.recent.insert(env.msg_id) {
            return;
        }
        self.dispatch(from, env);
    }

    fn decrypt_existing(
        &mut self,
        from: PeerId,
        header: &Header,
        ct: &[u8],
    ) -> Result<Vec<u8>, CoreError> {
        let peer = self.peers.get_mut(&from).ok_or(CoreError::Crypto)?;
        let pt = peer.ratchet.decrypt(header, ct, b"", &mut self.rng)?;
        peer.pending_init = None;
        Ok(pt)
    }

    fn accept_init(
        &mut self,
        from: PeerId,
        init: &InitHeader,
        header: &Header,
        ct: &[u8],
    ) -> Result<Vec<u8>, CoreError> {
        if let Some(p) = self.peers.get_mut(&from)
            && p.accepted_ephemerals.contains(&init.ephemeral)
        {
            return Ok(p.ratchet.decrypt(header, ct, b"", &mut self.rng)?);
        }

        let spk = self.prekeys.spk(init.spk_id).ok_or(CoreError::Crypto)?;
        let opk = match init.opk_id {
            Some(id) => Some(self.prekeys.opk(id).ok_or(CoreError::Crypto)?),
            None => None,
        };
        let mut ratchet = handshake::respond(&self.identity, spk, opk, &from, init)?;
        let pt = ratchet.decrypt(header, ct, b"", &mut self.rng)?;

        if let Some(existing) = self.peers.get(&from)
            && existing.is_unconfirmed_initiator()
            && self.peer_id < from
        {
            return Err(CoreError::Conflict);
        }
        if let Some(id) = init.opk_id {
            self.prekeys.take_opk(id);
            self.persist_prekeys();
        }

        let previous = self.peers.remove(&from);
        let is_new = previous.is_none();
        let (inbox, mut seen) = previous
            .map(|p| (p.inbox, p.accepted_ephemerals))
            .unwrap_or_default();
        Peer::remember_ephemeral(&mut seen, init.ephemeral);
        self.peers.insert(
            from,
            Peer {
                ratchet,
                pending_init: None,
                accepted_ephemerals: seen,
                identity_dh: init.identity_dh,
                inbox,
                hello_sent: false,
            },
        );
        self.requeue_for(&from);
        if is_new {
            self.emit(Event::PeerAdded {
                peer: from,
                initiated_by_us: false,
            });
        }
        self.send_hello(from);
        Ok(pt)
    }

    fn dispatch(&mut self, from: PeerId, env: Envelope) {
        let Envelope {
            msg_id,
            sent_at_ms,
            body,
        } = env;
        match body {
            Body::Hello { inbox } => {
                if let Some(p) = self.peers.get_mut(&from) {
                    p.inbox = Some(inbox);
                }
                self.persist_peer(&from);
                self.send_hello(from);
            }
            Body::Text { text, reply_to } => {
                self.store_message(StoredMessage {
                    msg_id,
                    peer: from,
                    outgoing: false,
                    sent_at_ms,
                    status: MessageStatus::Delivered,
                    content: Content::Text { text, reply_to },
                });
                self.send_receipt(from, msg_id);
            }
            Body::File { desc, kind } => {
                self.on_file_offer(from, msg_id, sent_at_ms, desc, kind);
                self.send_receipt(from, msg_id);
            }
            Body::FileCtl(ctl) => self.on_file_ctl(from, ctl),
            Body::Receipt { kind, ids } => {
                let status = match kind {
                    ReceiptKind::Delivered => MessageStatus::Delivered,
                    ReceiptKind::Read => MessageStatus::Read,
                };
                for id in ids {
                    self.set_status(id, status);
                }
            }
        }
    }

    fn send_receipt(&mut self, peer: PeerId, id: MsgId) {
        self.send_control(
            peer,
            Body::Receipt {
                kind: ReceiptKind::Delivered,
                ids: vec![id],
            },
        );
    }
}
