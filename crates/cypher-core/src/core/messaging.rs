use bytes::Bytes;
use cypher_crypto::{DeviceList, Header, InitHeader, PrekeyBundle, handshake};
use cypher_types::{Addr, DeviceId, MsgId, PeerId};
use cypher_wire::ClientMsg;
use rand_core::CryptoRngCore;
use zeroize::Zeroizing;

use super::{Core, Pending};
use crate::CoreError;
use crate::api::{Content, Event, FailReason, MessageStatus, StoredMessage};
use crate::envelope::{Body, Envelope, MAX_RECEIPT_IDS, MAX_TEXT_LEN, ReceiptKind, SyncBody};
use crate::peer::{OwnLinks, Peer, ProfileRecord, Session, clean_name};
use crate::relay::RelayBody;
use crate::share::ShareLink;
use crate::store::{META_LINKS, META_PROFILE, StoreOp, Table, message_key, session_key};

/// How long an invite admits a contact; the server keeps links as long.
const LINK_TTL_MS: u64 = 24 * 3600 * 1000;
/// Invites remembered at once; the oldest go first.
const MAX_OWN_LINKS: usize = 32;
/// Strangers waiting for an answer at once; more are not let in.
const MAX_PENDING_REQUESTS: usize = 20;

/// An invite being joined by, while the host's devices answer one by one.
pub(super) struct Joining {
    link: String,
    /// Key lookups not answered yet.
    outstanding: usize,
    /// The host's devices, to remember once the host is a contact.
    devices: Option<DeviceList>,
    /// The UI was told the contact is there.
    added: bool,
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
        let content = stored.content.clone();
        self.store_message(stored);
        self.sync_sent(peer, msg_id, content);
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
            self.sync(SyncBody::Read {
                peer,
                ids: chunk.to_vec(),
            });
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
        if !self.adopt_profile_name(name) {
            return;
        }
        self.sync(SyncBody::Profile {
            name: self.profile_name.clone(),
        });
        let mut greeted: Vec<Addr> = self
            .sessions
            .iter()
            .filter(|(addr, s)| {
                s.hello_sent
                    && self
                        .peers
                        .get(&addr.peer)
                        .is_some_and(|p| !p.request && !p.blocked)
            })
            .map(|(addr, _)| *addr)
            .collect();
        greeted.sort_unstable();
        for addr in greeted {
            let hello = self.hello(&addr.peer);
            self.send_control_to(addr, hello);
        }
    }

    /// Keeps the name this user goes by; whether it changed. Contacts hear
    /// of it from the device it was changed on.
    pub(super) fn adopt_profile_name(&mut self, name: Option<&str>) -> bool {
        let name = name.and_then(clean_name);
        if name == self.profile_name {
            return false;
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
        true
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
    pub(super) fn remember_link(&mut self, code: &str, at_ms: u64) {
        let now = self.now;
        self.own_links
            .retain(|(c, at)| now.saturating_sub(*at) < LINK_TTL_MS && c != code);
        if now.saturating_sub(at_ms) >= LINK_TTL_MS {
            return self.persist_links();
        }
        self.own_links.push((code.to_owned(), at_ms));
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

    pub(super) fn persist_links(&mut self) {
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
        let mut devices: Vec<Addr> = self
            .sessions
            .keys()
            .filter(|a| a.peer == *peer)
            .copied()
            .collect();
        devices.sort_unstable();
        for addr in devices {
            self.send_hello(addr);
        }
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
            self.drop_outbox_for(peer);
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
        let devices: Vec<Addr> = self
            .sessions
            .keys()
            .filter(|a| a.peer == *peer)
            .copied()
            .collect();
        for addr in devices {
            self.sessions.remove(&addr);
            self.persist(StoreOp::Delete {
                table: Table::Sessions,
                key: session_key(addr),
            });
        }
        self.drop_outbox_for(peer);
        self.cancel_transfers_with(peer);
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

    /// The host of an invite we are joining by has these devices (its first
    /// only, as far as anyone knows, without a list): ask for each one's keys.
    pub(super) fn join_devices(&mut self, link: &str, peer: PeerId, list: Option<DeviceList>) {
        let devices = list
            .as_ref()
            .map_or_else(|| vec![DeviceId::FIRST], |l| l.devices().to_vec());
        let joining = Joining {
            link: link.to_owned(),
            outstanding: devices.len(),
            devices: list,
            added: false,
        };
        self.joining.insert(peer, joining);
        for device in devices {
            let addr = Addr::new(peer, device);
            let link = link.to_owned();
            self.request(
                ClientMsg::FetchKeys { peer, device },
                Pending::FetchKeys { link, addr },
                false,
            );
        }
    }

    /// Keys of one of the host's devices: a session with it.
    pub(super) fn on_keys(
        &mut self,
        link: &str,
        addr: Addr,
        base: &[u8],
        opk: Option<(u32, [u8; 32])>,
    ) {
        let Some((mut fresh, identity_dh)) = self.initiate(addr, base, opk) else {
            return self.join_answered(addr.peer, Some(FailReason::InvalidKeys));
        };
        let peer = addr.peer;
        let settled = self.peers.contains_key(&peer)
            && self
                .sessions
                .get(&addr)
                .is_some_and(|s| !s.is_unconfirmed_initiator());
        if !settled {
            if let Some(old) = self.sessions.remove(&addr) {
                fresh.carry_over(old);
            }
            self.sessions.insert(addr, fresh);
            let devices = self.joining.get_mut(&peer).and_then(|j| j.devices.take());
            // We asked for this contact, by this invite.
            let p = self
                .peers
                .entry(peer)
                .or_insert_with(|| Peer::new(identity_dh));
            p.request = false;
            p.via = ShareLink::parse(link).map(|share| share.link().as_str().to_owned());
            if p.devices.is_none() {
                p.devices = devices;
            }
            self.persist_peer(&peer);
            self.persist_session(addr);
            self.requeue_for(addr);
            self.send_hello(addr);
            self.sync_contact(peer);
        }
        self.join_answered(peer, None);
    }

    pub(super) fn on_join_keys_failed(&mut self, peer: PeerId, reason: FailReason) {
        self.join_answered(peer, Some(reason));
    }

    /// One of the host's devices answered, or could not be reached. The UI
    /// hears once that the contact is there, or that joining failed when no
    /// device of theirs could be.
    fn join_answered(&mut self, peer: PeerId, failure: Option<FailReason>) {
        let Some(joining) = self.joining.get_mut(&peer) else {
            return;
        };
        joining.outstanding = joining.outstanding.saturating_sub(1);
        let first = failure.is_none() && !joining.added;
        joining.added |= first;
        let (finished, added, link) = (
            joining.outstanding == 0,
            joining.added,
            joining.link.clone(),
        );
        if finished {
            self.joining.remove(&peer);
        }
        if first {
            self.emit(Event::PeerAdded {
                peer,
                initiated_by_us: true,
            });
        } else if finished && !added {
            let reason = failure.unwrap_or(FailReason::ServerError);
            self.emit(Event::JoinFailed { link, reason });
        }
    }

    /// A fresh session with one device of a contact: one we had none with,
    /// or one whose messages stopped decrypting. What waits for that device
    /// goes out on it; theirs follow once it accepts.
    pub(super) fn on_session_keys(
        &mut self,
        addr: Addr,
        base: &[u8],
        opk: Option<(u32, [u8; 32])>,
    ) {
        if addr.peer != self.peer_id && !self.peers.contains_key(&addr.peer) {
            return;
        }
        let Some((mut fresh, _)) = self.initiate(addr, base, opk) else {
            return;
        };
        if let Some(old) = self.sessions.remove(&addr) {
            fresh.carry_over(old);
        }
        self.sessions.insert(addr, fresh);
        self.persist_session(addr);
        self.requeue_for(addr);
        self.send_hello(addr);
    }

    /// Our side of a new session with a device from its published bundle,
    /// and the identity key that bundle carries.
    fn initiate(
        &mut self,
        addr: Addr,
        base: &[u8],
        opk: Option<(u32, [u8; 32])>,
    ) -> Option<(Session, [u8; 32])> {
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
            .filter(|b| b.identity == addr.peer && b.device == addr.device)?;
        let (ratchet, init) =
            handshake::initiate(&self.identity, self.device, &bundle, &mut self.rng).ok()?;
        Some((Session::new(ratchet, Some(init)), bundle.identity_dh))
    }

    /// Greets one device of a contact, once per session; a request or a
    /// blocked contact hears nothing.
    fn send_hello(&mut self, addr: Addr) {
        let open = self
            .peers
            .get(&addr.peer)
            .is_some_and(|p| !p.request && !p.blocked);
        let Some(s) = self.sessions.get_mut(&addr) else {
            return;
        };
        if s.hello_sent || !open {
            return;
        }
        s.hello_sent = true;
        self.persist_session(addr);
        let hello = self.hello(&addr.peer);
        self.send_control_to(addr, hello);
    }

    pub(super) fn on_relay(&mut self, sender: Addr, body: &Bytes, via_inbox: bool) {
        match RelayBody::decode(body) {
            Ok(RelayBody::Message {
                init,
                header,
                ciphertext,
            }) => self.on_message(sender, init.as_ref(), &header, &ciphertext),
            Ok(RelayBody::Chunk {
                file_id,
                index,
                data,
            }) if !via_inbox => {
                self.on_chunk(sender, file_id, index, &data);
            }
            Ok(RelayBody::Ack {
                file_id,
                next,
                sack,
                tag,
            }) if !via_inbox => self.on_file_ack(sender, file_id, next, sack, &tag),
            _ => {}
        }
    }

    fn on_message(&mut self, sender: Addr, init: Option<&InitHeader>, header: &Header, ct: &[u8]) {
        let from = sender.peer;
        if self.peers.get(&from).is_some_and(|p| p.blocked) {
            return;
        }
        let result = match init {
            Some(init) => self.accept_init(sender, init, header, ct),
            None => self.decrypt_existing(sender, header, ct),
        };
        let plaintext = match result {
            Ok(pt) => Zeroizing::new(pt),
            Err(CoreError::Conflict) => return,
            Err(_) => {
                self.emit(Event::Warning {
                    reason: FailReason::DecryptFailed,
                });
                if init.is_none() {
                    self.repair_session(sender);
                }
                return;
            }
        };
        self.persist_session(sender);
        let Ok(env) = Envelope::decode(&plaintext) else {
            return;
        };
        if !self.recent.insert(env.msg_id) {
            // Sent again because our receipt was lost: confirm it again.
            if matches!(env.body, Body::Text { .. } | Body::File { .. }) {
                self.send_receipt(from, env.msg_id);
            }
            return;
        }
        self.dispatch(sender, env);
        // A device their identity has not listed (to our knowledge): their
        // list may have grown.
        if from == self.peer_id {
            self.check_own_device(sender.device);
        } else if self
            .peers
            .get(&from)
            .is_some_and(|p| !p.lists(sender.device))
        {
            self.refresh_devices(from);
        }
    }

    fn decrypt_existing(
        &mut self,
        sender: Addr,
        header: &Header,
        ct: &[u8],
    ) -> Result<Vec<u8>, CoreError> {
        let session = self.sessions.get_mut(&sender).ok_or(CoreError::Crypto)?;
        let pt = session.ratchet.decrypt(header, ct, b"", &mut self.rng)?;
        session.pending_init = None;
        Ok(pt)
    }

    fn accept_init(
        &mut self,
        sender: Addr,
        init: &InitHeader,
        header: &Header,
        ct: &[u8],
    ) -> Result<Vec<u8>, CoreError> {
        let Addr {
            peer: from,
            device: from_device,
        } = sender;
        if let Some(s) = self.sessions.get_mut(&sender)
            && s.accepted_ephemerals.contains(&init.ephemeral)
        {
            return Ok(s.ratchet.decrypt(header, ct, b"", &mut self.rng)?);
        }

        let own = from == self.peer_id;
        if !own
            && !self.peers.contains_key(&from)
            && self.pending_requests() >= MAX_PENDING_REQUESTS
        {
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
        let mut ratchet = handshake::respond(
            &self.identity,
            self.device,
            spk,
            opk,
            (&from, from_device),
            init,
        )?;
        let pt = ratchet.decrypt(header, ct, b"", &mut self.rng)?;

        // Both sides started a session at once: the lower device keeps its
        // own and ignores the other's.
        if self
            .sessions
            .get(&sender)
            .is_some_and(Session::is_unconfirmed_initiator)
            && (self.peer_id, self.device) < (from, from_device)
        {
            return Err(CoreError::Conflict);
        }
        if let Some(id) = init.opk_id {
            self.prekeys.take_opk(id);
            self.persist_prekeys();
        }

        if !own {
            self.meet(from, init.identity_dh);
        }
        let mut fresh = Session::new(ratchet, None);
        if let Some(old) = self.sessions.remove(&sender) {
            fresh.carry_over(old);
        }
        fresh.remember_ephemeral(init.ephemeral);
        self.sessions.insert(sender, fresh);
        self.requeue_for(sender);
        self.send_hello(sender);
        Ok(pt)
    }

    /// Someone new is a request until their `Hello` names one of our
    /// invites; someone known keeps what they were.
    fn meet(&mut self, from: PeerId, identity_dh: [u8; 32]) {
        let p = self.peers.entry(from).or_insert_with(|| {
            let mut stranger = Peer::new(identity_dh);
            stranger.request = true;
            stranger
        });
        p.identity_dh = identity_dh;
        self.persist_peer(&from);
    }

    /// One device of a contact greets us: its inbox, the name they go by,
    /// and the invite they joined by, which makes a stranger a contact.
    fn on_hello(&mut self, sender: Addr, inbox: [u8; 32], name: Option<&str>, via: Option<&str>) {
        let from = sender.peer;
        let invited = via.is_some_and(|code| self.take_own_link(code));
        let name = name.and_then(clean_name);
        let mut renamed = false;
        if let Some(s) = self.sessions.get_mut(&sender) {
            s.inbox = Some(inbox);
        }
        if let Some(p) = self.peers.get_mut(&from) {
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
        self.persist_session(sender);
        self.send_hello(sender);
    }

    fn dispatch(&mut self, sender: Addr, env: Envelope) {
        let from = sender.peer;
        let Envelope {
            msg_id,
            sent_at_ms,
            body,
        } = env;
        // Our own devices only keep each other in step; only they may.
        if from == self.peer_id {
            match body {
                Body::Sync(sync) => self.on_sync(sender, sync),
                Body::Devices { list, .. } => self.on_own_list_body(&list),
                Body::Hello { .. }
                | Body::Text { .. }
                | Body::File { .. }
                | Body::FileCtl(_)
                | Body::Receipt { .. } => {}
            }
            return;
        }
        match body {
            Body::Hello { inbox, name, via } => {
                self.on_hello(sender, inbox, name.as_deref(), via.as_deref());
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
                self.on_file_offer(sender, msg_id, sent_at_ms, desc, kind);
                self.send_receipt(from, msg_id);
            }
            Body::FileCtl(ctl) => self.on_file_ctl(sender, ctl),
            Body::Receipt { kind, ids } => self.on_receipt(sender, kind, ids),
            Body::Devices { list, inboxes } => self.on_devices_body(from, &list, inboxes),
            Body::Sync(_) => {}
        }
    }

    /// The contact got (or read) our messages: they stop waiting, and their
    /// status moves on, never back from "read".
    /// One device of a contact confirms our messages: they are done there.
    fn on_receipt(&mut self, sender: Addr, kind: ReceiptKind, ids: Vec<MsgId>) {
        let status = match kind {
            ReceiptKind::Delivered => MessageStatus::Delivered,
            ReceiptKind::Read => MessageStatus::Read,
        };
        for id in ids {
            if self.outbox.get(&id).is_some_and(|i| i.peer == sender.peer) {
                self.drop_target(id, sender.device);
            }
            if status == MessageStatus::Read {
                self.read.insert(id);
            } else if self.read.contains(&id) {
                continue;
            }
            self.report(id, status);
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
