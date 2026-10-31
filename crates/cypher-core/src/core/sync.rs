//! Keeping the devices of one identity alike: each tells the others what
//! the user did on it, and a new device asks them for what they know.

use std::collections::BTreeSet;
use std::collections::hash_map::Entry;

use cypher_crypto::DeviceList;
use cypher_types::{Addr, MsgId, PeerId};
use rand_core::CryptoRngCore;

use super::Core;
use crate::api::{Content, Event, MessageStatus, StoredMessage};
use crate::envelope::{Body, ContactState, MAX_STATE_CONTACTS, StatePart, SyncBody};
use crate::peer::{Peer, clean_name};

/// First wait before a new device asks its siblings again; it doubles with
/// each ask.
const STATE_ASK_MS: u64 = 60_000;
/// Asks while the app runs, the first included: the last one goes about an
/// hour after the first.
const MAX_STATE_ASKS: u32 = 7;

/// Parts received from one sibling for one state reply. This is deliberately
/// not persisted: a restart asks again if the reply was not completed.
#[derive(Debug)]
pub(super) struct StateReply {
    id: MsgId,
    total: u32,
    parts: BTreeSet<u32>,
}

impl StateReply {
    fn new(part: StatePart) -> Self {
        Self {
            id: part.id,
            total: part.total,
            parts: BTreeSet::new(),
        }
    }
}

/// A complete state reply, split into envelopes that each fit an inbox item.
fn state_bodies(
    profile: Option<&str>,
    links: &[(String, u64)],
    contacts: &[ContactState],
    id: MsgId,
) -> Vec<SyncBody> {
    let mut parts: Vec<Vec<ContactState>> = contacts
        .chunks(MAX_STATE_CONTACTS)
        .map(<[ContactState]>::to_vec)
        .collect();
    if parts.is_empty() {
        parts.push(Vec::new());
    }
    let total = u32::try_from(parts.len()).unwrap_or(u32::MAX);
    parts
        .into_iter()
        .enumerate()
        .map(|(n, contacts)| {
            let first = n == 0;
            SyncBody::State {
                profile: if first {
                    profile.map(str::to_owned)
                } else {
                    None
                },
                contacts,
                links: if first { links.to_vec() } else { Vec::new() },
                part: Some(StatePart {
                    id,
                    index: u32::try_from(n).unwrap_or(u32::MAX),
                    total,
                }),
            }
        })
        .collect()
}

impl<R: CryptoRngCore> Core<R> {
    /// Tells this identity's other devices; nothing when there are none.
    pub(super) fn sync(&mut self, body: SyncBody) {
        if !self.devices_of(self.peer_id).is_empty() {
            self.send_control(self.peer_id, Body::Sync(body));
        }
    }

    /// A message the user sent from this device.
    pub(super) fn sync_sent(&mut self, peer: PeerId, msg_id: MsgId, content: Content) {
        let sent_at_ms = self.now;
        self.sync(SyncBody::Sent {
            peer,
            msg_id,
            sent_at_ms,
            content,
        });
    }

    /// A contact as this device keeps it now.
    pub(super) fn sync_contact(&mut self, peer: PeerId) {
        if let Some(state) = self.contact_state(peer) {
            self.sync(SyncBody::Contact(state));
        }
    }

    fn contact_state(&self, peer: PeerId) -> Option<ContactState> {
        let p = self.peers.get(&peer)?;
        Some(ContactState {
            peer,
            identity_dh: p.identity_dh,
            alias: p.alias.clone(),
            name: p.name.clone(),
            request: p.request,
            blocked: p.blocked,
            via: p.via.clone(),
            devices: p.devices.as_ref().map(DeviceList::encode),
        })
    }

    /// The user forgot a contact on this device, and so on every device.
    pub(super) fn forget_contact(&mut self, peer: PeerId) {
        self.remove_peer(&peer);
        self.sync(SyncBody::Removed { peer });
    }

    pub(super) fn rename_contact(&mut self, peer: PeerId, alias: Option<&str>) {
        self.rename_peer(&peer, alias);
        self.sync_contact(peer);
    }

    pub(super) fn accept_request(&mut self, peer: PeerId) {
        self.accept_contact(&peer);
        self.sync_contact(peer);
    }

    pub(super) fn block_contact(&mut self, peer: PeerId, blocked: bool) {
        self.set_blocked(&peer, blocked);
        self.sync_contact(peer);
    }

    /// A device new to its identity's list asks its siblings for what they
    /// know, whoever else (a contact greeting it first) it knows already:
    /// once each time the app starts, then again as [`ask_siblings_again`]
    /// says, until one of them answers.
    ///
    /// [`ask_siblings_again`]: Self::ask_siblings_again
    pub(super) fn ask_siblings(&mut self) {
        if self.devices.awaiting_state && self.devices.state_asks == 0 {
            self.send_state_request();
        }
    }

    /// The siblings did not answer (the request was lost, or turned away
    /// as two sessions crossed): asked again after a minute, then twice as
    /// long each time, [`MAX_STATE_ASKS`] times in all while the app runs.
    pub(super) fn ask_siblings_again(&mut self) {
        let due = self.devices.awaiting_state
            && (1..MAX_STATE_ASKS).contains(&self.devices.state_asks)
            && self.now >= self.devices.next_state_ask;
        if due {
            self.send_state_request();
        }
    }

    fn send_state_request(&mut self) {
        if self.devices_of(self.peer_id).is_empty() {
            return;
        }
        let asks = self.devices.state_asks;
        let wait = STATE_ASK_MS.saturating_mul(1 << asks.min(MAX_STATE_ASKS));
        self.devices.state_asks = asks.saturating_add(1);
        self.devices.next_state_ask = self.now.saturating_add(wait);
        self.sync(SyncBody::StateRequest);
    }

    /// What another device of this identity says.
    pub(super) fn on_sync(&mut self, sender: Addr, body: SyncBody) {
        match body {
            SyncBody::Sent {
                peer,
                msg_id,
                sent_at_ms,
                content,
            } => self.on_sent(StoredMessage {
                msg_id,
                peer,
                outgoing: true,
                sent_at_ms,
                status: MessageStatus::Sent,
                content,
            }),
            SyncBody::Read { peer, ids } => {
                if self.peers.contains_key(&peer) {
                    for id in ids {
                        self.set_status(id, MessageStatus::Read);
                    }
                }
            }
            SyncBody::Contact(state) => {
                self.adopt_contact(state);
                self.emit(Event::ContactsChanged);
            }
            SyncBody::Removed { peer } => {
                self.forget_held(&peer);
                self.remove_peer(&peer);
                self.emit(Event::ContactsChanged);
            }
            SyncBody::Profile { name } => {
                self.adopt_profile_name(name.as_deref());
            }
            SyncBody::Link { code, at_ms } => self.adopt_link(&code, at_ms),
            SyncBody::StateRequest => {
                self.send_state(sender);
                self.serve_history(sender.device);
            }
            SyncBody::History { desc } => self.on_history_offer(sender, desc),
            SyncBody::State {
                profile,
                contacts,
                links,
                part,
            } => self.on_state(sender, profile.as_deref(), contacts, links, part),
        }
    }

    /// A sibling's state, or one marked part of it. Contents take effect as
    /// they arrive, but only a whole reply ends the retries.
    fn on_state(
        &mut self,
        sender: Addr,
        profile: Option<&str>,
        contacts: Vec<ContactState>,
        links: Vec<(String, u64)>,
        part: Option<StatePart>,
    ) {
        let complete = match part {
            Some(part) if self.devices.awaiting_state => self.state_part_complete(sender, part),
            Some(_) => false,
            // Older siblings did not mark parts. Keep accepting their replies
            // as before; marked replies complete only whole.
            None => true,
        };
        if profile.is_some() {
            self.adopt_profile_name(profile);
        }
        for contact in contacts {
            self.adopt_contact(contact);
        }
        for (code, at_ms) in links {
            self.adopt_link(&code, at_ms);
        }
        // Persist completion after the last part's contents, so a crash
        // cannot leave a completed flag over missing contacts.
        if complete && self.devices.awaiting_state {
            self.devices.awaiting_state = false;
            self.persist_own_devices();
        }
        self.emit(Event::ContactsChanged);
    }

    /// Whether this part completes a reply. A new reply starts at part zero;
    /// a later part of another reply cannot complete the one being tracked.
    fn state_part_complete(&mut self, sender: Addr, part: StatePart) -> bool {
        let reply = match self.devices.state_replies.entry(sender) {
            Entry::Vacant(slot) => {
                if part.index != 0 {
                    return false;
                }
                slot.insert(StateReply::new(part))
            }
            Entry::Occupied(slot) => {
                let reply = slot.into_mut();
                if reply.id != part.id || reply.total != part.total {
                    if part.index != 0 {
                        return false;
                    }
                    *reply = StateReply::new(part);
                }
                reply
            }
        };
        reply.parts.insert(part.index);
        let complete = reply.parts.len() == part.total as usize;
        if complete {
            self.devices.state_replies.remove(&sender);
        }
        complete
    }

    /// A message another device of ours sent: shown here as sent, and this
    /// one takes the contact's receipts for it too. A device setting up
    /// may hear of it before it knows the contact: then it waits for them.
    fn on_sent(&mut self, msg: StoredMessage) {
        let (peer, msg_id) = (msg.peer, msg.msg_id);
        if self.peers.contains_key(&peer) {
            self.store_message(msg);
            self.await_receipts(peer, msg_id);
        } else {
            self.hold_sent(msg);
        }
    }

    /// Everything a new device should know, in parts that each fit an
    /// inbox item.
    fn send_state(&mut self, to: Addr) {
        let mut peers: Vec<PeerId> = self.peers.keys().copied().collect();
        peers.sort_unstable();
        let contacts: Vec<ContactState> = peers
            .into_iter()
            .filter_map(|p| self.contact_state(p))
            .collect();
        let id = self.random_id();
        let bodies = state_bodies(self.profile_name.as_deref(), &self.own_links, &contacts, id);
        for body in bodies {
            self.send_control_to(to, Body::Sync(body));
        }
    }

    /// A contact as another device keeps it: added here when new, made the
    /// same otherwise. Sessions with their devices come when first needed.
    fn adopt_contact(&mut self, state: ContactState) {
        if state.peer == self.peer_id {
            return;
        }
        let devices = state
            .devices
            .as_deref()
            .and_then(|l| DeviceList::decode(l).ok())
            .filter(|l| l.identity() == state.peer);
        let p = self
            .peers
            .entry(state.peer)
            .or_insert_with(|| Peer::new(state.identity_dh));
        let was_blocked = p.blocked;
        p.alias = state.alias.as_deref().and_then(clean_name);
        p.name = state.name.as_deref().and_then(clean_name);
        p.request = state.request;
        p.blocked = state.blocked;
        p.via = state.via;
        self.persist_peer(&state.peer);
        if let Some(list) = devices {
            self.learn_devices(state.peer, list);
        }
        if state.blocked && !was_blocked {
            self.drop_outbox_for(&state.peer);
            self.cancel_transfers_with(&state.peer);
        }
    }

    fn adopt_link(&mut self, code: &str, at_ms: u64) {
        if !self.own_links.iter().any(|(c, _)| c == code) {
            self.remember_link(code, at_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(n: u8) -> ContactState {
        ContactState {
            peer: PeerId([n; 32]),
            identity_dh: [n.wrapping_add(1); 32],
            alias: None,
            name: None,
            request: false,
            blocked: false,
            via: None,
            devices: None,
        }
    }

    #[test]
    fn state_replies_split_only_their_contacts() {
        for count in [0_u8, 50, 51, 100] {
            let contacts: Vec<_> = (0..count).map(contact).collect();
            let expected_total = usize::from(count).div_ceil(MAX_STATE_CONTACTS).max(1);
            let bodies = state_bodies(
                Some("Alice"),
                &[("invite".into(), 7)],
                &contacts,
                MsgId([9; 16]),
            );
            assert_eq!(bodies.len(), expected_total);
            let mut read = Vec::new();
            for (n, body) in bodies.into_iter().enumerate() {
                let SyncBody::State {
                    profile,
                    contacts,
                    links,
                    part,
                } = body
                else {
                    panic!("state_bodies returns only State");
                };
                assert_eq!(profile.as_deref(), (n == 0).then_some("Alice"));
                assert_eq!(links.len(), usize::from(n == 0));
                assert!(contacts.len() <= MAX_STATE_CONTACTS);
                assert_eq!(
                    part,
                    Some(StatePart {
                        id: MsgId([9; 16]),
                        index: u32::try_from(n).unwrap(),
                        total: u32::try_from(expected_total).unwrap(),
                    })
                );
                read.extend(contacts);
            }
            assert_eq!(read, contacts);
        }
    }
}
