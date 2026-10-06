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
use crate::peer::{OwnLinks, Peer, ProfileRecord, clean_name};
use crate::relay::{self, RelayBody};
use crate::share::ShareLink;
use crate::store::{META_LINKS, META_PROFILE, StoreOp, Table, message_key};

/// First wait before retrying a message, by why it did not go out; each
/// further attempt doubles it, up to [`MAX_RETRY_MS`].
const RETRY_OFFLINE_MS: u64 = 30_000;
const RETRY_BUSY_MS: u64 = 2_000;
const RETRY_FAILED_MS: u64 = 5_000;
const MAX_RETRY_MS: u64 = 10 * 60_000;
/// Doublings before the backoff stops growing (2^10 × the base > the cap).
const MAX_DOUBLINGS: u32 = 10;
/// How long an invite admits a contact; the server keeps links as long.
const LINK_TTL_MS: u64 = 24 * 3600 * 1000;
/// Invites remembered at once; the oldest go first.
const MAX_OWN_LINKS: usize = 32;
/// Strangers waiting for an answer at once; more are not let in.
const MAX_PENDING_REQUESTS: usize = 20;

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
    /// Failed attempts since the app started; resets on restart, which is
    /// a reasonable moment to try again promptly.
    #[serde(skip)]
    attempts: u32,
}

impl crate::Record for OutboxItem {
    const VERSION: u8 = 1;
}

impl Drop for OutboxItem {
    fn drop(&mut self) {
        self.envelope.zeroize();
    }
}

/// Wait before the next attempt after `attempts` failures: `base` doubled
/// per failure up to [`MAX_RETRY_MS`], then a point in its upper half picked
/// by `roll`.
fn retry_delay(base: u64, attempts: u32, roll: u64) -> u64 {
    let backoff = base
        .saturating_mul(1 << attempts.min(MAX_DOUBLINGS))
        .min(MAX_RETRY_MS);
    let half = backoff / 2;
    half + roll % (half + 1)
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

    /// Tells `peer` its messages were read and remembers that locally, so
    /// the UI sees them as read and does not report them again.
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
        for &msg_id in ids {
            self.set_status(msg_id, MessageStatus::Read);
        }
    }

    pub(super) fn rename_peer(&mut self, peer: &PeerId, alias: Option<&str>) {
        let Some(p) = self.peers.get_mut(peer) else {
            return;
        };
        p.alias = alias.and_then(clean_name);
        self.persist_peer(peer);
    }

    /// Sets the name this user goes by and tells every contact already
    /// greeted; the others hear it in their first `Hello`.
    pub(super) fn set_profile_name(&mut self, name: Option<&str>) {
        let name = name.and_then(clean_name);
        if name == self.profile_name {
            return;
        }
        self.profile_name = name;
        let op = self.vault.put(
            Table::Meta,
            META_PROFILE.to_vec(),
            &ProfileRecord {
                name: self.profile_name.clone(),
            },
            &mut self.rng,
        );
        self.persist(op);
        let mut greeted: Vec<PeerId> = self
            .peers
            .iter()
            .filter(|(_, p)| p.hello_sent && !p.request && !p.blocked)
            .map(|(id, _)| *id)
            .collect();
        greeted.sort_unstable_by_key(|id| *id.as_bytes());
        for peer in greeted {
            let hello = self.hello(&peer);
            self.send_control(peer, hello);
        }
    }

    fn hello(&self, peer: &PeerId) -> Body {
        Body::Hello {
            inbox: self.inbox_id,
            name: self.profile_name.clone(),
            via: self.peers.get(peer).and_then(|p| p.via.clone()),
        }
    }

    /// Remembers an invite this user made, so whoever joins by it is taken
    /// as a contact.
    pub(super) fn remember_link(&mut self, code: &str) {
        let now = self.now;
        self.own_links
            .retain(|(_, at)| now.saturating_sub(*at) < LINK_TTL_MS);
        self.own_links.push((code.to_owned(), now));
        if self.own_links.len() > MAX_OWN_LINKS {
            self.own_links.remove(0);
        }
        self.persist_links();
    }

    /// Whether `code` is a live invite of ours; it admits one contact only.
    fn take_own_link(&mut self, code: &str) -> bool {
        let now = self.now;
        let Some(i) = self
            .own_links
            .iter()
            .position(|(c, at)| c == code && now.saturating_sub(*at) < LINK_TTL_MS)
        else {
            return false;
        };
        self.own_links.remove(i);
        self.persist_links();
        true
    }

    fn persist_links(&mut self) {
        let record = OwnLinks {
            links: self.own_links.clone(),
        };
        let op = self
            .vault
            .put(Table::Meta, META_LINKS.to_vec(), &record, &mut self.rng);
        self.persist(op);
    }

    fn pending_requests(&self) -> usize {
        self.peers.values().filter(|p| p.request).count()
    }

    /// Takes a stranger as a contact: from now on they hear our `Hello`.
    pub(super) fn accept_contact(&mut self, peer: &PeerId) {
        let Some(p) = self.peers.get_mut(peer) else {
            return;
        };
        if !p.request {
            return;
        }
        p.request = false;
        self.persist_peer(peer);
        self.send_hello(*peer);
    }

    /// Blocking drops whatever they send, unread, and stops what goes to
    /// them; unblocking lets them through again.
    pub(super) fn set_blocked(&mut self, peer: &PeerId, blocked: bool) {
        let Some(p) = self.peers.get_mut(peer) else {
            return;
        };
        p.blocked = blocked;
        self.persist_peer(peer);
        if blocked {
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
            attempts: 0,
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

    /// Schedules another attempt: `base` doubled per earlier failure, capped,
    /// then drawn from its upper half so that clients cut off together do
    /// not all come back at the same moment.
    fn retry_later(&mut self, msg_id: MsgId, base: u64) {
        let roll = self.rng.next_u64();
        if let Some(item) = self.outbox.get_mut(&msg_id) {
            item.in_flight = false;
            item.next_try = self.now + retry_delay(base, item.attempts, roll);
            item.attempts = item.attempts.saturating_add(1);
        }
    }

    fn complete_outbox(&mut self, msg_id: MsgId, status: MessageStatus) {
        let tracked = self.outbox.get(&msg_id).is_some_and(|i| i.tracked);
        self.drop_outbox(&msg_id);
        if tracked {
            self.set_status(msg_id, status);
        }
    }

    pub(super) fn discard_outgoing(&mut self, msg_id: &MsgId) {
        self.drop_outbox(msg_id);
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
        let Some(mut fresh) = self.initiate(peer, base, opk) else {
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
        if let Some(old) = self.peers.remove(&peer) {
            fresh.carry_over(old);
        }
        // We asked for this contact, by this invite.
        fresh.request = false;
        fresh.via = ShareLink::parse(&link).map(|share| share.link().as_str().to_owned());
        self.peers.insert(peer, fresh);
        self.requeue_for(&peer);
        self.emit(Event::PeerAdded {
            peer,
            initiated_by_us: true,
        });
        self.send_hello(peer);
    }

    /// A fresh session with a contact whose messages stopped decrypting.
    /// Messages still waiting go out on it; theirs follow once they accept.
    pub(super) fn on_repair_keys(
        &mut self,
        peer: PeerId,
        base: &[u8],
        opk: Option<(u32, [u8; 32])>,
    ) {
        if !self.peers.contains_key(&peer) {
            return;
        }
        let Some(mut fresh) = self.initiate(peer, base, opk) else {
            return;
        };
        if let Some(old) = self.peers.remove(&peer) {
            fresh.carry_over(old);
        }
        self.peers.insert(peer, fresh);
        self.requeue_for(&peer);
        self.send_hello(peer);
    }

    /// Our side of a new session with `peer` from its published bundle.
    fn initiate(
        &mut self,
        peer: PeerId,
        base: &[u8],
        opk: Option<(u32, [u8; 32])>,
    ) -> Option<Peer> {
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
        let bundle = PrekeyBundle::decode(&raw)
            .ok()
            .filter(|b| b.identity == peer)?;
        let (ratchet, init) = handshake::initiate(&self.identity, &bundle, &mut self.rng).ok()?;
        Some(Peer::new(ratchet, bundle.identity_dh, Some(init)))
    }

    fn send_hello(&mut self, peer: PeerId) {
        let Some(p) = self.peers.get_mut(&peer) else {
            return;
        };
        if p.hello_sent || p.request || p.blocked {
            return;
        }
        p.hello_sent = true;
        self.persist_peer(&peer);
        let hello = self.hello(&peer);
        self.send_control(peer, hello);
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
        if self.peers.get(&from).is_some_and(|p| p.blocked) {
            return;
        }
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
                if init.is_none() {
                    self.repair_session(from);
                }
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

        if !self.peers.contains_key(&from) && self.pending_requests() >= MAX_PENDING_REQUESTS {
            // Quietly: a flood of strangers is not the user's problem.
            return Err(CoreError::Conflict);
        }
        let known = self.prekeys.spk(init.spk_id).is_some()
            && init.opk_id.is_none_or(|id| self.prekeys.opk(id).is_some());
        if !known {
            self.resync_prekeys();
            return Err(CoreError::Crypto);
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

        // Someone new is a request until their `Hello` names one of our
        // invites; someone known keeps what they were.
        let mut fresh = Peer::new(ratchet, init.identity_dh, None);
        fresh.request = true;
        if let Some(old) = self.peers.remove(&from) {
            fresh.carry_over(old);
        }
        Peer::remember_ephemeral(&mut fresh.accepted_ephemerals, init.ephemeral);
        self.peers.insert(from, fresh);
        self.requeue_for(&from);
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
            Body::Hello { inbox, name, via } => {
                let invited = via.as_deref().is_some_and(|code| self.take_own_link(code));
                let name = name.as_deref().and_then(clean_name);
                let mut renamed = false;
                if let Some(p) = self.peers.get_mut(&from) {
                    p.inbox = Some(inbox);
                    renamed = p.name != name;
                    p.name.clone_from(&name);
                }
                if renamed {
                    self.emit(Event::PeerProfile { peer: from, name });
                }
                match self.peers.get_mut(&from) {
                    Some(p) if p.request && invited => {
                        p.request = false;
                        self.emit(Event::PeerAdded {
                            peer: from,
                            initiated_by_us: false,
                        });
                    }
                    Some(p) if p.request => self.emit(Event::ContactRequest { peer: from }),
                    _ => {}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_back_off_exponentially_with_jitter_up_to_a_cap() {
        // `roll` 0 gives the bottom of the range, `roll` = half its top.
        let bounds = |attempts, half| {
            (
                retry_delay(RETRY_FAILED_MS, attempts, 0),
                retry_delay(RETRY_FAILED_MS, attempts, half),
            )
        };
        assert_eq!(
            bounds(0, 2_500),
            (2_500, 5_000),
            "the first wait is the base, jittered"
        );
        assert_eq!(bounds(1, 5_000), (5_000, 10_000), "each failure doubles it");
        assert_eq!(
            bounds(30, MAX_RETRY_MS / 2),
            (MAX_RETRY_MS / 2, MAX_RETRY_MS),
            "until the cap"
        );
        for roll in [1, 7, 12_345, u64::MAX / 3, u64::MAX] {
            let d = retry_delay(RETRY_BUSY_MS, 3, roll);
            assert!((8_000..=16_000).contains(&d), "{d}");
        }
    }
}
