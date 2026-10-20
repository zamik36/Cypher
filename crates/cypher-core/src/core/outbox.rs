//! End-to-end messages on their way to a contact's devices: each device a
//! message goes to is a target with its own path (online, inbox, retries)
//! and its own receipt.

use bytes::Bytes;
use cypher_crypto::sealed;
use cypher_types::{Addr, DeviceId, MsgId, PeerId};
use cypher_wire::{ClientMsg, DeliveryStatus, ErrorCode};
use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use super::anon::Readiness;
use super::{Core, Pending};
use crate::CoreError;
use crate::api::MessageStatus;
use crate::envelope::{Body, Envelope};
use crate::peer::Session;
use crate::relay;
use crate::store::{StoreOp, Table};

/// First wait before retrying a message, by why it did not go out; each
/// further attempt doubles it, up to [`MAX_RETRY_MS`].
const RETRY_OFFLINE_MS: u64 = 30_000;
const RETRY_BUSY_MS: u64 = 2_000;
const RETRY_FAILED_MS: u64 = 5_000;
const MAX_RETRY_MS: u64 = 10 * 60_000;
/// Doublings before the backoff stops growing (2^10 × the base > the cap).
const MAX_DOUBLINGS: u32 = 10;
/// Messages to one contact kept for a receipt; older ones are let go.
const MAX_AWAITING_PER_PEER: usize = 500;
/// How long messages to a device go as ourselves after its token was
/// used up for the hour.
const SEALED_PAUSE_MS: u64 = 60 * 60_000;
/// How long they do after the server could not hand a sealed message to
/// the device's gateway at all.
const SEALED_BROKEN_MS: u64 = 5 * 60_000;

/// An end-to-end message on its way. Stored as padded plaintext and
/// encrypted at send time for each device, so a session reset never
/// strands it. A message the user sees stays after the server took it,
/// until each device's receipt: if a device could not read it, a fresh
/// session with that device sends it again.
#[derive(Serialize, Deserialize)]
pub(crate) struct OutboxItem {
    pub msg_id: MsgId,
    pub peer: PeerId,
    envelope: Vec<u8>,
    /// User-visible message whose status the UI tracks.
    tracked: bool,
    /// The contact's devices it has yet to reach, or to hear back from.
    targets: Vec<Target>,
    /// The best status told so far: a late answer about one device cannot
    /// take back what another device's already told.
    reported: MessageStatus,
}

/// How one copy of a message goes to its device.
enum Route {
    Sealed(SealedRoute),
    /// It would go sealed, but the relay is not up yet.
    Later,
    /// Over the session, with our name.
    Named,
}

/// How a message reaches a contact's device without our name: the
/// device's access token, the inbox to leave it in if the device is away,
/// and the key to seal it to.
struct SealedRoute {
    token: [u8; 32],
    inbox: [u8; 32],
    identity_dh: [u8; 32],
}

/// One device a message goes to.
#[derive(Serialize, Deserialize)]
struct Target {
    device: DeviceId,
    /// Taken by the server; waiting for this device's receipt.
    awaiting: bool,
    #[serde(skip)]
    in_flight: bool,
    #[serde(skip)]
    next_try: u64,
    /// Failed attempts since the app started; resets on restart, which is
    /// a reasonable moment to try again promptly.
    #[serde(skip)]
    attempts: u32,
}

impl Target {
    fn new(device: DeviceId) -> Self {
        Self {
            device,
            awaiting: false,
            in_flight: false,
            next_try: 0,
            attempts: 0,
        }
    }

    fn due(&self, now: u64) -> bool {
        !self.in_flight && !self.awaiting && self.next_try <= now
    }
}

impl OutboxItem {
    fn target(&mut self, device: DeviceId) -> Option<&mut Target> {
        self.targets.iter_mut().find(|t| t.device == device)
    }

    fn awaiting(&self) -> bool {
        self.targets.iter().any(|t| t.awaiting)
    }

    /// No device is left to send it to: only its receipts, or the record
    /// that it was read, are kept.
    fn settled(&self) -> bool {
        self.targets.is_empty()
    }

    /// Which of the messages waiting for one contact's receipts go first
    /// when there are too many: those already read, then those only a read
    /// receipt is left for, then the oldest still on their way.
    fn trim_order(&self) -> (u8, u64, MsgId) {
        match (self.settled(), self.reported) {
            (true, MessageStatus::Read) => (0, 0, self.msg_id),
            (true, _) => (1, 0, self.msg_id),
            (false, _) => {
                let sent = Envelope::decode(&self.envelope).map_or(0, |e| e.sent_at_ms);
                (2, sent, self.msg_id)
            }
        }
    }

    /// The devices it still goes to.
    pub(super) fn devices(&self) -> impl Iterator<Item = DeviceId> + '_ {
        self.targets.iter().map(|t| t.device)
    }
}

impl crate::Record for OutboxItem {
    const VERSION: u8 = 3;

    /// Versions 1 and 2 went to the contact's one device; version 1 was
    /// dropped once the server took it.
    fn upgrade(version: u8, body: &[u8]) -> Result<Self, CoreError> {
        #[derive(Deserialize)]
        struct V1 {
            msg_id: MsgId,
            peer: PeerId,
            envelope: Vec<u8>,
            tracked: bool,
        }
        #[derive(Deserialize)]
        struct V2 {
            msg_id: MsgId,
            peer: PeerId,
            envelope: Vec<u8>,
            tracked: bool,
            awaiting: bool,
        }
        let parse_err = |_| CoreError::Storage;
        let mut old: V2 = match version {
            1 => {
                let v1: V1 = postcard::from_bytes(body).map_err(parse_err)?;
                V2 {
                    msg_id: v1.msg_id,
                    peer: v1.peer,
                    envelope: v1.envelope,
                    tracked: v1.tracked,
                    awaiting: false,
                }
            }
            2 => postcard::from_bytes(body).map_err(parse_err)?,
            _ => return Err(CoreError::Storage),
        };
        let mut target = Target::new(DeviceId::FIRST);
        target.awaiting = old.awaiting;
        Ok(Self {
            msg_id: old.msg_id,
            peer: old.peer,
            envelope: std::mem::take(&mut old.envelope),
            tracked: old.tracked,
            targets: vec![target],
            reported: MessageStatus::Pending,
        })
    }
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

/// How far along a status is: one only ever moves forward.
fn rank(status: MessageStatus) -> u8 {
    match status {
        MessageStatus::Pending | MessageStatus::Failed => 0,
        MessageStatus::Queued => 1,
        MessageStatus::Sent => 2,
        MessageStatus::Delivered => 3,
        MessageStatus::Read => 4,
    }
}

impl<R: CryptoRngCore> Core<R> {
    /// Sends a protocol message that has no UI representation to every
    /// device of `peer`.
    pub(super) fn send_control(&mut self, peer: PeerId, body: Body) {
        let devices = self.devices_of(peer);
        let id = self.random_id();
        self.enqueue_to(peer, &devices, id, body, false);
    }

    /// Sends a protocol message that has no UI representation to one device.
    pub(super) fn send_control_to(&mut self, addr: Addr, body: Body) {
        let id = self.random_id();
        self.enqueue_to(addr.peer, &[addr.device], id, body, false);
    }

    pub(super) fn random_id(&mut self) -> MsgId {
        let mut id = [0u8; 16];
        self.rng.fill_bytes(&mut id);
        MsgId(id)
    }

    /// Sends a message to every device of `peer`.
    pub(super) fn enqueue(&mut self, peer: PeerId, msg_id: MsgId, body: Body, tracked: bool) {
        let devices = self.devices_of(peer);
        self.enqueue_to(peer, &devices, msg_id, body, tracked);
    }

    fn enqueue_to(
        &mut self,
        peer: PeerId,
        devices: &[DeviceId],
        msg_id: MsgId,
        body: Body,
        tracked: bool,
    ) {
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
            targets: devices.iter().copied().map(Target::new).collect(),
            reported: MessageStatus::Pending,
        };
        let op = self
            .vault
            .put(Table::Outbox, msg_id.to_vec(), &item, &mut self.rng);
        self.persist(op);
        self.outbox.insert(msg_id, item);
        self.send_due(msg_id);
    }

    pub(super) fn flush_outbox(&mut self) {
        if !self.is_ready() {
            return;
        }
        let due: Vec<MsgId> = self
            .outbox
            .values()
            .filter(|i| i.targets.iter().any(|t| t.due(self.now)))
            .map(|i| i.msg_id)
            .collect();
        for id in due {
            self.send_due(id);
        }
    }

    /// Sends one message to each of its devices that is due.
    fn send_due(&mut self, msg_id: MsgId) {
        let Some(item) = self.outbox.get(&msg_id) else {
            return;
        };
        let due: Vec<DeviceId> = item
            .targets
            .iter()
            .filter(|t| t.due(self.now))
            .map(|t| t.device)
            .collect();
        for device in due {
            self.send_to(msg_id, device);
        }
    }

    /// Encrypts and sends a message to one device. The advanced ratchet is
    /// persisted before the ciphertext is handed to the transport, so a
    /// crash can never lead to message-key reuse. A device without a
    /// session yet gets one from its published keys first.
    fn send_to(&mut self, msg_id: MsgId, device: DeviceId) {
        if !self.is_ready() {
            return;
        }
        let Some(peer) = self.outbox.get(&msg_id).map(|item| item.peer) else {
            return;
        };
        let addr = Addr::new(peer, device);
        let route = self.sealed_route(addr);
        // Still due: it goes as soon as the relay is up (or given up on).
        if matches!(route, Route::Later) {
            return;
        }
        let Some(item) = self.outbox.get_mut(&msg_id) else {
            return;
        };
        let Some(session) = self.sessions.get_mut(&addr) else {
            if let Some(target) = item.target(device) {
                // Keys are on their way; the new session sends it at once.
                target.next_try = self.now.saturating_add(RETRY_OFFLINE_MS);
            }
            self.open_session(addr);
            return;
        };
        let Some(target) = item.targets.iter_mut().find(|t| t.device == device) else {
            return;
        };
        if !target.due(self.now) || !session.ratchet.can_send() {
            return;
        }
        let Ok((header, ct)) = session.ratchet.encrypt(&item.envelope, b"") else {
            return;
        };
        let body = Bytes::from(relay::message_body(
            session.pending_init.as_ref(),
            &header,
            &ct,
        ));
        target.in_flight = true;

        self.persist_session(addr);
        if let Route::Sealed(route) = route
            && self.send_sealed(msg_id, addr, &route, &body)
        {
            return;
        }
        self.send_named(msg_id, addr, body);
    }

    /// Sends `body` to `addr` over the session: the gateway stamps it with
    /// our name.
    fn send_named(&mut self, msg_id: MsgId, addr: Addr, body: Bytes) {
        let req_id = self.alloc_req();
        self.pending.insert(
            req_id,
            (
                Pending::Send {
                    msg_id,
                    addr,
                    body: body.clone(),
                },
                self.now.saturating_add(super::REQUEST_TIMEOUT_MS),
            ),
        );
        self.transmit(
            req_id,
            ClientMsg::Send {
                to: addr.peer,
                device: addr.device,
                want_ack: true,
                body,
            },
        );
    }

    /// Sends `body` to `addr` by `route`, without our name; whether it
    /// went.
    fn send_sealed(
        &mut self,
        msg_id: MsgId,
        addr: Addr,
        route: &SealedRoute,
        body: &Bytes,
    ) -> bool {
        let Some(item) = self.seal_item(&route.identity_dh, body) else {
            return false;
        };
        let msg = ClientMsg::SendSealed {
            to: addr.peer,
            device: addr.device,
            token: route.token,
            inbox: Some(route.inbox),
            item,
        };
        let body = body.clone();
        self.request(msg, Pending::SendSealed { msg_id, addr, body }, true);
        true
    }

    /// How a message to `addr` goes: without our name to a contact's
    /// device that told us its access token, while its token is not off,
    /// once the relay is up; while the relay is still coming up, it waits
    /// rather than go with our name.
    fn sealed_route(&self, addr: Addr) -> Route {
        if addr.peer == self.peer_id {
            return Route::Named;
        }
        let Some(peer) = self.peers.get(&addr.peer) else {
            return Route::Named;
        };
        let Some(session) = self.sessions.get(&addr) else {
            return Route::Named;
        };
        let ad = session.inbox.or_else(|| peer.inbox_of(addr.device));
        let (Some(ad), true) = (ad, session.sealed_off_until <= self.now) else {
            return Route::Named;
        };
        match self.anon.readiness(self.now) {
            Readiness::Onion => {
                let epoch = self.inbox_epoch();
                Route::Sealed(SealedRoute {
                    token: ad.token_at(epoch),
                    inbox: ad.at(epoch),
                    identity_dh: peer.identity_dh,
                })
            }
            Readiness::Wait => Route::Later,
            Readiness::Session | Readiness::Unavailable => Route::Named,
        }
    }

    /// The server would not deliver a sealed message: with a refused or
    /// used-up token, or no way to the device's gateway, it goes as
    /// ourselves at once; otherwise later.
    pub(super) fn on_sealed_refused(&mut self, msg_id: MsgId, addr: Addr, code: ErrorCode) {
        let off_until = match code {
            ErrorCode::Unauthorized => u64::MAX,
            ErrorCode::RateLimited => self.now.saturating_add(SEALED_PAUSE_MS),
            ErrorCode::Unavailable => self.now.saturating_add(SEALED_BROKEN_MS),
            ErrorCode::NotFound => return self.on_inbox_gone(msg_id, addr),
            ErrorCode::BadRequest
            | ErrorCode::TooLarge
            | ErrorCode::Internal
            | ErrorCode::UnsupportedVersion
            | ErrorCode::Conflict => return self.on_send_failed(msg_id, addr.device),
        };
        if let Some(s) = self.sessions.get_mut(&addr) {
            s.sealed_off_until = off_until;
        }
        let now = self.now;
        if let Some(target) = self
            .outbox
            .get_mut(&msg_id)
            .and_then(|item| item.target(addr.device))
        {
            target.in_flight = false;
            target.next_try = now;
        }
        self.send_to(msg_id, addr.device);
    }

    pub(super) fn on_send_ack(
        &mut self,
        msg_id: MsgId,
        addr: Addr,
        body: &Bytes,
        status: DeliveryStatus,
    ) {
        match status {
            DeliveryStatus::Delivered => {
                // Only a message the contact always answers (with a receipt)
                // tells that silence means trouble: a host does not answer
                // the Hello of someone it has not accepted yet.
                let answered = self.outbox.get(&msg_id).is_some_and(|i| i.tracked);
                if answered
                    && self
                        .sessions
                        .get(&addr)
                        .is_some_and(Session::is_unconfirmed_initiator)
                {
                    self.init_heard.entry(addr).or_insert(self.now);
                }
                self.complete_target(msg_id, addr.device, MessageStatus::Sent);
            }
            DeliveryStatus::Offline => match self.inbox_of(addr) {
                Some((inbox, identity_dh)) => {
                    self.put_inbox(msg_id, addr, inbox, &identity_dh, body);
                }
                None => self.retry_later(msg_id, addr.device, RETRY_OFFLINE_MS),
            },
            DeliveryStatus::Busy => self.retry_later(msg_id, addr.device, RETRY_BUSY_MS),
            DeliveryStatus::Queued => self.on_inbox_queued(msg_id, addr.device),
        }
    }

    /// Where to leave a message for a device that is away, and the key to
    /// seal it to: the inbox its `Hello` or its identity's announcement told,
    /// or, for our own devices, the one this identity derives for it.
    fn inbox_of(&self, addr: Addr) -> Option<([u8; 32], [u8; 32])> {
        if addr.peer == self.peer_id {
            let own_dh = self.identity.dh_public_key().to_bytes();
            return Some((self.own_inbox_of(addr.device), own_dh));
        }
        let peer = self.peers.get(&addr.peer)?;
        let ad = self
            .sessions
            .get(&addr)
            .and_then(|s| s.inbox)
            .or_else(|| peer.inbox_of(addr.device))?;
        Some((ad.at(self.inbox_epoch()), peer.identity_dh))
    }

    pub(super) fn on_send_failed(&mut self, msg_id: MsgId, device: DeviceId) {
        self.retry_later(msg_id, device, RETRY_FAILED_MS);
    }

    pub(super) fn on_inbox_queued(&mut self, msg_id: MsgId, device: DeviceId) {
        self.complete_target(msg_id, device, MessageStatus::Queued);
    }

    fn put_inbox(
        &mut self,
        msg_id: MsgId,
        addr: Addr,
        inbox: [u8; 32],
        identity_dh: &[u8; 32],
        body: &[u8],
    ) {
        match self.anon.readiness(self.now) {
            Readiness::Onion | Readiness::Session => {}
            Readiness::Wait => return self.retry_later(msg_id, addr.device, RETRY_BUSY_MS),
            Readiness::Unavailable => {
                return self.retry_later(msg_id, addr.device, RETRY_OFFLINE_MS);
            }
        }
        match self.seal_item(identity_dh, body) {
            Some(item) => self.request(
                ClientMsg::InboxPut { inbox, item },
                Pending::InboxPut { msg_id, addr },
                true,
            ),
            None => self.retry_later(msg_id, addr.device, RETRY_OFFLINE_MS),
        }
    }

    /// `body` sealed to a contact's identity key with who sends it inside,
    /// as an inbox item or a sealed delivery carries it.
    fn seal_item(&mut self, identity_dh: &[u8; 32], body: &[u8]) -> Option<Bytes> {
        let mut plain = Zeroizing::new(Vec::with_capacity(36 + body.len()));
        plain.extend_from_slice(self.peer_id.as_bytes());
        plain.extend_from_slice(&self.device.0.to_le_bytes());
        plain.extend_from_slice(body);
        sealed::seal(identity_dh, &plain, &mut self.rng)
            .ok()
            .map(Bytes::from)
    }

    /// Schedules another attempt: `base` doubled per earlier failure, capped,
    /// then drawn from its upper half so that clients cut off together do
    /// not all come back at the same moment.
    fn retry_later(&mut self, msg_id: MsgId, device: DeviceId, base: u64) {
        let roll = self.rng.next_u64();
        let now = self.now;
        if let Some(target) = self
            .outbox
            .get_mut(&msg_id)
            .and_then(|item| item.target(device))
        {
            target.in_flight = false;
            target.next_try = now.saturating_add(retry_delay(base, target.attempts, roll));
            target.attempts = target.attempts.saturating_add(1);
        }
    }

    /// The server took the message for one device. Control traffic is done
    /// there; a message the user sees waits for that device's receipt.
    fn complete_target(&mut self, msg_id: MsgId, device: DeviceId, status: MessageStatus) {
        let Some(item) = self.outbox.get_mut(&msg_id) else {
            return;
        };
        if !item.tracked {
            self.drop_target(msg_id, device);
            return;
        }
        let Some(target) = item.target(device) else {
            return;
        };
        target.awaiting = true;
        target.in_flight = false;
        let peer = item.peer;
        self.persist_outbox(&msg_id);
        self.report(msg_id, status);
        self.trim_awaiting(&peer);
    }

    /// The inbox was closed or never opened. Most likely what we know of it
    /// is old and the device tells its new one soon: try again later, and
    /// check that the device is still listed.
    pub(super) fn on_inbox_gone(&mut self, msg_id: MsgId, addr: Addr) {
        self.retry_later(msg_id, addr.device, RETRY_OFFLINE_MS);
        if addr.peer != self.peer_id {
            self.refresh_devices(addr.peer);
        }
    }

    /// A device answered for a message (a receipt), or will never need to:
    /// the message is done there.
    pub(super) fn drop_target(&mut self, msg_id: MsgId, device: DeviceId) {
        let Some(item) = self.outbox.get_mut(&msg_id) else {
            return;
        };
        item.targets.retain(|t| t.device != device);
        self.settle(msg_id);
    }

    /// Stores what is left of a message after a change. Once no device is
    /// left to send it to, a message the user sees that some device took
    /// stays, without its content: their receipts are taken only for
    /// messages sent to them, and once it is read it stays read, whatever
    /// late receipt or repeated news of it comes. Anything else is done
    /// then.
    fn settle(&mut self, msg_id: MsgId) {
        let Some(item) = self.outbox.get_mut(&msg_id) else {
            return;
        };
        let kept = item.tracked
            && matches!(
                item.reported,
                MessageStatus::Queued
                    | MessageStatus::Sent
                    | MessageStatus::Delivered
                    | MessageStatus::Read
            );
        if !item.settled() {
            self.persist_outbox(&msg_id);
        } else if kept {
            item.envelope.zeroize();
            item.envelope.shrink_to_fit();
            self.persist_outbox(&msg_id);
        } else {
            self.drop_outbox(&msg_id);
        }
    }

    /// A device of the contact got, or read, a message we sent them. A
    /// receipt for anything else is not theirs to give: another
    /// conversation's message, or an id made up to fill the store.
    pub(super) fn confirm(&mut self, sender: Addr, msg_id: MsgId, status: MessageStatus) {
        let Some(item) = self
            .outbox
            .get_mut(&msg_id)
            .filter(|i| i.peer == sender.peer && i.tracked)
        else {
            return;
        };
        let targets = item.targets.len();
        item.targets.retain(|t| t.device != sender.device);
        let advanced = rank(status) > rank(item.reported);
        if advanced {
            item.reported = status;
        }
        if advanced || item.targets.len() != targets {
            self.settle(msg_id);
        }
        if advanced {
            self.set_status(msg_id, status);
        }
    }

    /// A message another device of ours sent: this one takes the contact's
    /// receipts for it too.
    pub(super) fn await_receipts(&mut self, peer: PeerId, msg_id: MsgId) {
        if self.outbox.contains_key(&msg_id) {
            return;
        }
        let item = OutboxItem {
            msg_id,
            peer,
            envelope: Vec::new(),
            tracked: true,
            targets: Vec::new(),
            reported: MessageStatus::Sent,
        };
        let op = self
            .vault
            .put(Table::Outbox, msg_id.to_vec(), &item, &mut self.rng);
        self.persist(op);
        self.outbox.insert(msg_id, item);
        self.trim_awaiting(&peer);
    }

    /// Forgets `addr` as a target of everything on its way.
    pub(super) fn drop_targets(&mut self, addr: Addr) {
        let ids: Vec<MsgId> = self
            .outbox
            .values()
            .filter(|i| i.peer == addr.peer && i.targets.iter().any(|t| t.device == addr.device))
            .map(|i| i.msg_id)
            .collect();
        for id in ids {
            self.drop_target(id, addr.device);
        }
    }

    /// Tells the UI a message got further, never back.
    fn report(&mut self, msg_id: MsgId, status: MessageStatus) {
        let Some(item) = self.outbox.get_mut(&msg_id) else {
            return;
        };
        if rank(status) <= rank(item.reported) {
            return;
        }
        item.reported = status;
        self.persist_outbox(&msg_id);
        self.set_status(msg_id, status);
    }

    /// Keeps at most [`MAX_AWAITING_PER_PEER`] messages waiting for one
    /// contact's receipts, letting go first as [`OutboxItem::trim_order`]
    /// says.
    fn trim_awaiting(&mut self, peer: &PeerId) {
        let waits = |i: &&OutboxItem| i.peer == *peer && (i.settled() || i.awaiting());
        let count = self.outbox.values().filter(waits).count();
        let Some(excess) = count.checked_sub(MAX_AWAITING_PER_PEER).filter(|n| *n > 0) else {
            return;
        };
        let mut waiting: Vec<(u8, u64, MsgId)> = self
            .outbox
            .values()
            .filter(waits)
            .map(OutboxItem::trim_order)
            .collect();
        waiting.select_nth_unstable(excess - 1);
        for (_, _, id) in waiting.into_iter().take(excess) {
            self.drop_outbox(&id);
        }
    }

    fn persist_outbox(&mut self, msg_id: &MsgId) {
        if let Some(item) = self.outbox.get(msg_id) {
            let op = self
                .vault
                .put(Table::Outbox, msg_id.to_vec(), item, &mut self.rng);
            self.persist(op);
        }
    }

    pub(super) fn discard_outgoing(&mut self, msg_id: &MsgId) {
        self.drop_outbox(msg_id);
    }

    pub(super) fn drop_outbox(&mut self, msg_id: &MsgId) {
        if self.outbox.remove(msg_id).is_some() {
            self.persist(StoreOp::Delete {
                table: Table::Outbox,
                key: msg_id.to_vec(),
            });
        }
    }

    /// Everything on its way to `peer`, for forgetting or blocking them.
    pub(super) fn drop_outbox_for(&mut self, peer: &PeerId) {
        let stale: Vec<MsgId> = self
            .outbox
            .values()
            .filter(|i| i.peer == *peer)
            .map(|i| i.msg_id)
            .collect();
        for id in stale {
            self.drop_outbox(&id);
        }
    }

    /// Sends what waits for one device again on a freshly established
    /// session: what is in flight, and what the server took but the device
    /// never confirmed (it may have been lost with the old session).
    pub(super) fn requeue_for(&mut self, addr: Addr) {
        let mut confirmed_none = Vec::new();
        for item in self.outbox.values_mut().filter(|i| i.peer == addr.peer) {
            let msg_id = item.msg_id;
            if let Some(target) = item.target(addr.device) {
                target.in_flight = false;
                target.next_try = 0;
                if target.awaiting {
                    target.awaiting = false;
                    confirmed_none.push(msg_id);
                }
            }
        }
        for id in confirmed_none {
            self.persist_outbox(&id);
        }
        // At once, so they arrive before anything written from now on.
        self.flush_outbox();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Effect, Event};
    use crate::core::Snapshot;
    use cypher_crypto::IdentitySeed;

    /// A message another device of ours sent, read by the contact, stays
    /// read: news of it repeated, a late receipt from another device of
    /// theirs, or a receipt from someone else change nothing.
    #[test]
    fn a_read_message_stays_read_whatever_comes_after() {
        let seed = IdentitySeed([1; 32]);
        let snapshot = Snapshot::default();
        let (mut core, _) =
            Core::restore(&seed, DeviceId::FIRST, &snapshot, 1_000, rand::rngs::OsRng).unwrap();
        let (peer, id) = (PeerId([2; 32]), MsgId([3; 16]));
        core.await_receipts(peer, id);
        core.confirm(Addr::new(peer, DeviceId::FIRST), id, MessageStatus::Read);
        core.await_receipts(peer, id);
        core.confirm(Addr::new(peer, DeviceId(2)), id, MessageStatus::Delivered);
        core.confirm(
            Addr::new(PeerId([4; 32]), DeviceId::FIRST),
            id,
            MessageStatus::Delivered,
        );
        let item = &core.outbox[&id];
        assert!(item.settled() && item.reported == MessageStatus::Read);
        let told: Vec<MessageStatus> = core
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::Emit(Event::MessageStatus { status, .. }) => Some(*status),
                _ => None,
            })
            .collect();
        assert_eq!(told, [MessageStatus::Read], "told once, never taken back");
    }

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

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig {
            cases: if cfg!(miri) { 8 } else { 512 },
            failure_persistence: None,
            ..proptest::prelude::ProptestConfig::default()
        })]

        /// Every wait lies between half its base and the cap, in the upper
        /// half of the doubled base; and a later attempt never waits less
        /// than an earlier one could, until both reach the cap.
        #[test]
        fn retry_delays_grow_within_their_bounds(
            base in proptest::sample::select(vec![RETRY_BUSY_MS, RETRY_FAILED_MS, RETRY_OFFLINE_MS]),
            attempts in 0u32..64,
            roll: u64,
        ) {
            let delay = retry_delay(base, attempts, roll);
            let least = retry_delay(base, attempts, 0);
            proptest::prop_assert!((base / 2..=MAX_RETRY_MS).contains(&delay));
            proptest::prop_assert!((least..=least * 2 + 1).contains(&delay));
            let next_least = retry_delay(base, attempts + 1, 0);
            if next_least < MAX_RETRY_MS / 2 {
                proptest::prop_assert!(next_least >= delay);
            } else {
                proptest::prop_assert!(next_least >= least);
            }
        }
    }

    #[test]
    fn statuses_only_move_forward() {
        let order = [
            MessageStatus::Pending,
            MessageStatus::Queued,
            MessageStatus::Sent,
            MessageStatus::Delivered,
            MessageStatus::Read,
        ];
        assert!(order.windows(2).all(|w| rank(w[0]) < rank(w[1])));
        assert_eq!(rank(MessageStatus::Failed), rank(MessageStatus::Pending));
    }

    /// Items from before devices went to the contact's one device: they load
    /// with that device as their only target, still waiting where they were.
    #[test]
    fn outbox_items_from_before_devices_go_to_the_first_device() {
        #[derive(Serialize)]
        struct V1<'a> {
            msg_id: MsgId,
            peer: PeerId,
            envelope: &'a [u8],
            tracked: bool,
        }
        #[derive(Serialize)]
        struct V2<'a> {
            msg_id: MsgId,
            peer: PeerId,
            envelope: &'a [u8],
            tracked: bool,
            awaiting: bool,
        }
        let vault = crate::Vault::new([4; 32]);
        let seal = |version: u8, body: Vec<u8>| {
            let plain = [vec![version], body].concat();
            vault.seal_bytes(Table::Outbox, b"k", &plain, &mut rand::rngs::OsRng)
        };
        let (msg_id, peer) = (MsgId([1; 16]), PeerId([2; 32]));
        let v1 = V1 {
            msg_id,
            peer,
            envelope: b"padded envelope",
            tracked: true,
        };
        let item: OutboxItem = vault
            .open(
                Table::Outbox,
                b"k",
                &seal(1, postcard::to_allocvec(&v1).unwrap()),
            )
            .unwrap();
        assert_eq!((item.msg_id, item.peer), (msg_id, peer));
        assert_eq!(item.envelope, b"padded envelope");
        assert!(item.tracked && !item.awaiting());
        assert_eq!(item.targets.len(), 1);
        assert_eq!(item.targets[0].device, DeviceId::FIRST);

        let v2 = V2 {
            msg_id,
            peer,
            envelope: b"e",
            tracked: true,
            awaiting: true,
        };
        let item: OutboxItem = vault
            .open(
                Table::Outbox,
                b"k",
                &seal(2, postcard::to_allocvec(&v2).unwrap()),
            )
            .unwrap();
        assert!(item.awaiting());

        let unknown = seal(7, postcard::to_allocvec(&v1).unwrap());
        assert!(
            vault
                .open::<OutboxItem>(Table::Outbox, b"k", &unknown)
                .is_err()
        );
    }
}
