//! Keeping the devices of one identity alike: each tells the others what
//! the user did on it, and a new device asks them for what they know.

use std::collections::BTreeSet;

use cypher_crypto::DeviceList;
use cypher_types::{Addr, MsgId, PeerId};
use rand_core::CryptoRngCore;

use super::Core;
use crate::api::{Content, Event, FailReason, MessageStatus, StoredMessage};
use crate::envelope::{
    Body, ContactState, MAX_STATE_CONTACTS, MAX_STATE_PARTS, StatePart, SyncBody,
};
use crate::peer::{Peer, clean_name};

/// First wait before a new device asks its siblings again; it doubles with
/// each ask.
const STATE_ASK_MS: u64 = 60_000;
/// Asks while the app runs, the first included: the last one goes about an
/// hour after the first.
const MAX_STATE_ASKS: u32 = 7;
/// Two replies can cross while a retry is in flight. A third is refused
/// rather than evicting either and mixing their completion state.
const MAX_STATE_REPLIES: usize = 2;

/// Parts received from one sibling for one state reply. This is deliberately
/// not persisted: a restart asks again if the reply was not completed.
#[derive(Debug)]
pub(super) struct StateReply {
    id: MsgId,
    request: Option<MsgId>,
    total: u32,
    parts: BTreeSet<u32>,
}

impl StateReply {
    fn new(part: StatePart) -> Self {
        Self {
            id: part.id,
            request: part.request,
            total: part.total,
            parts: BTreeSet::new(),
        }
    }
}

/// More contacts than a complete, bounded state reply can represent.
#[derive(Debug)]
struct StateOverflow;

/// A complete state reply, split into envelopes that each fit an inbox item.
/// Refuse an unrepresentable reply rather than sending a plausible prefix.
fn state_bodies(
    profile: Option<&str>,
    links: &[(String, u64)],
    contacts: &[ContactState],
    id: MsgId,
    request: Option<MsgId>,
) -> Result<Vec<SyncBody>, StateOverflow> {
    if contacts.len() > MAX_STATE_CONTACTS * MAX_STATE_PARTS as usize {
        return Err(StateOverflow);
    }
    let mut parts: Vec<Vec<ContactState>> = contacts
        .chunks(MAX_STATE_CONTACTS)
        .map(<[ContactState]>::to_vec)
        .collect();
    if parts.is_empty() {
        parts.push(Vec::new());
    }
    let total = u32::try_from(parts.len()).unwrap_or(u32::MAX);
    Ok(parts
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
                    request,
                }),
            }
        })
        .collect())
}

impl<R: CryptoRngCore> Core<R> {
    /// Tells this identity's other devices; nothing when there are none.
    pub(super) fn sync(&mut self, body: SyncBody) {
        if !self.devices_of(self.me.peer_id).is_empty() {
            self.send_control(self.me.peer_id, Body::Sync(body));
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
        if !self.devices.awaiting_state {
            return;
        }
        if self.devices.state_asks >= MAX_STATE_ASKS {
            if self.now >= self.devices.next_state_ask {
                self.devices.state_replies.clear();
            }
            return;
        }
        if self.devices.state_asks > 0 && self.now >= self.devices.next_state_ask {
            self.send_state_request();
        }
    }

    fn send_state_request(&mut self) {
        if self.devices_of(self.me.peer_id).is_empty() {
            return;
        }
        let request = self.random_id();
        let asks = self.devices.state_asks;
        let wait = STATE_ASK_MS.saturating_mul(1 << asks.min(MAX_STATE_ASKS));
        self.devices.state_asks = asks.saturating_add(1);
        self.devices.next_state_ask = self.now.saturating_add(wait);
        self.devices.state_request = Some(request);
        self.devices.state_replies.clear();
        self.sync(SyncBody::StateRequest {
            request: Some(request),
        });
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
            SyncBody::StateRequest { request } => {
                self.send_state(sender, request);
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
            // An unmarked response proves nothing about a correlated request.
            // With no such request, retain the old best-effort compatibility.
            None => self.devices.state_request.is_none(),
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
            self.devices.state_replies.clear();
        }
        self.emit(Event::ContactsChanged);
    }

    /// Tracks two responses to the current request per sibling. Duplicate
    /// parts are idempotent; a third response ID cannot displace either one.
    /// Older or uncorrelated responses still apply their contents elsewhere,
    /// but never enter this tracker and never complete the current request.
    fn state_part_complete(&mut self, sender: Addr, part: StatePart) -> bool {
        if !part.valid() {
            return false;
        }
        match (self.devices.state_request, part.request) {
            (Some(current), Some(request)) if current == request => {}
            // A legacy exchange has no request on either side. There is no
            // ordering information to infer, so any whole marked reply does.
            (None, None) => {}
            _ => return false,
        }
        let replies = self.devices.state_replies.entry(sender).or_default();
        let index = match replies.iter().position(|reply| reply.id == part.id) {
            Some(index) => index,
            None if replies.len() >= MAX_STATE_REPLIES => return false,
            None => {
                replies.push(StateReply::new(part));
                replies.len() - 1
            }
        };
        let Some(reply) = replies.get_mut(index) else {
            return false;
        };
        if reply.request != part.request || reply.total != part.total {
            return false;
        }
        reply.parts.insert(part.index);
        reply.parts.len() == part.total as usize
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
    /// inbox item. No part goes out if the complete state cannot fit.
    fn send_state(&mut self, to: Addr, request: Option<MsgId>) {
        let mut peers: Vec<PeerId> = self.peers.keys().copied().collect();
        peers.sort_unstable();
        let contacts: Vec<ContactState> = peers
            .into_iter()
            .filter_map(|p| self.contact_state(p))
            .collect();
        let id = self.random_id();
        let Ok(bodies) = state_bodies(
            self.profile.name.as_deref(),
            &self.profile.own_links,
            &contacts,
            id,
            request,
        ) else {
            self.emit(Event::Warning {
                reason: FailReason::TooLarge,
            });
            return;
        };
        for body in bodies {
            self.send_control_to(to, Body::Sync(body));
        }
    }

    /// A contact as another device keeps it: added here when new, made the
    /// same otherwise. Sessions with their devices come when first needed.
    fn adopt_contact(&mut self, state: ContactState) {
        if state.peer == self.me.peer_id {
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
        if !self.profile.own_links.iter().any(|(c, _)| c == code) {
            self.remember_link(code, at_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Effect;
    use crate::envelope::Envelope;

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
                Some(MsgId([7; 16])),
            )
            .expect("a small state reply fits");
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
                        request: Some(MsgId([7; 16])),
                    })
                );
                read.extend(contacts);
            }
            assert_eq!(read, contacts);
        }
    }

    fn waiting_core(now: u64) -> Core<rand::rngs::OsRng> {
        use super::super::Snapshot;
        use cypher_types::DeviceId;

        let seed = cypher_crypto::IdentitySeed([1; 32]);
        let (mut core, _) = Core::restore(
            &seed,
            DeviceId(2),
            &Snapshot::default(),
            now,
            rand::rngs::OsRng,
        )
        .unwrap();
        core.devices.own =
            Some(DeviceList::sign(&core.me.identity, 1, [DeviceId::FIRST, DeviceId(2)]).unwrap());
        core.devices.awaiting_state = true;
        core
    }

    fn requested_core(now: u64) -> Core<rand::rngs::OsRng> {
        let mut core = waiting_core(now);
        core.ask_siblings();
        core
    }

    fn current_request(core: &Core<rand::rngs::OsRng>) -> MsgId {
        core.devices.state_request.unwrap()
    }

    fn outbox_state_requests(core: &Core<rand::rngs::OsRng>) -> Vec<MsgId> {
        core.outbox
            .items
            .values()
            .filter_map(|item| {
                let envelope = Envelope::decode(item.envelope()).ok()?;
                let Body::Sync(SyncBody::StateRequest {
                    request: Some(request),
                }) = envelope.body
                else {
                    return None;
                };
                Some(request)
            })
            .collect()
    }

    fn assert_due_state_request(
        core: &Core<rand::rngs::OsRng>,
        requests: &mut Vec<MsgId>,
        expected: u32,
        minute: u64,
    ) {
        let after = outbox_state_requests(core);
        assert_eq!(after.len(), expected as usize, "due at {minute}");
        assert!(requests.iter().all(|id| after.contains(id)));
        assert_eq!(
            after.iter().filter(|id| !requests.contains(id)).count(),
            1,
            "one new request due at {minute}"
        );
        assert_eq!(
            after.iter().collect::<BTreeSet<_>>().len(),
            after.len(),
            "request IDs are unique at {minute}"
        );
        *requests = after;
    }

    fn state_part(request: Option<MsgId>, id: u8, index: u32, total: u32) -> StatePart {
        StatePart {
            id: MsgId([id; 16]),
            index,
            total,
            request,
        }
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "mirrors the optional part argument"
    )]
    fn part(id: u8, index: u32, total: u32) -> Option<StatePart> {
        Some(state_part(None, id, index, total))
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "mirrors the optional part argument"
    )]
    fn reply(request: MsgId, id: u8, index: u32, total: u32) -> Option<StatePart> {
        Some(state_part(Some(request), id, index, total))
    }

    #[test]
    fn state_tracking_bounds_parts_and_response_ids() {
        let mut core = waiting_core(0);
        let sender = Addr::new(core.me.peer_id, cypher_types::DeviceId::FIRST);
        core.on_state(
            sender,
            None,
            Vec::new(),
            Vec::new(),
            part(1, 0, MAX_STATE_PARTS + 1),
        );
        assert!(core.devices.state_replies.is_empty());

        core.on_state(
            sender,
            None,
            Vec::new(),
            Vec::new(),
            part(1, 0, MAX_STATE_PARTS),
        );
        core.on_state(sender, None, Vec::new(), Vec::new(), part(2, 1, 2));
        core.on_state(sender, None, Vec::new(), Vec::new(), part(3, 1, 2));
        let replies = &core.devices.state_replies[&sender];
        assert_eq!(replies.len(), MAX_STATE_REPLIES);
        assert_eq!(replies[0].id, MsgId([1; 16]));
        assert_eq!(replies[0].parts.len(), 1);
        assert_eq!(replies[1].id, MsgId([2; 16]));
    }

    #[test]
    fn the_first_complete_reply_clears_every_siblings_parts() {
        let mut core = waiting_core(0);
        let first = Addr::new(core.me.peer_id, cypher_types::DeviceId::FIRST);
        let second = Addr::new(core.me.peer_id, cypher_types::DeviceId(3));
        core.on_state(first, None, Vec::new(), Vec::new(), part(1, 0, 2));
        core.on_state(second, None, Vec::new(), Vec::new(), part(2, 0, 2));
        assert_eq!(core.devices.state_replies.len(), 2);

        core.on_state(first, None, Vec::new(), Vec::new(), part(1, 1, 2));
        assert!(!core.devices.awaiting_state);
        assert!(core.devices.state_replies.is_empty());
    }

    #[test]
    fn only_the_current_response_completes_when_old_parts_arrive_late() {
        let mut core = requested_core(0);
        let sender = Addr::new(core.me.peer_id, cypher_types::DeviceId::FIRST);
        let current = current_request(&core);
        let old = MsgId([250; 16]);
        assert_ne!(old, current);

        // The old A1, new B0, late A0 sequence used to complete A and stop
        // listening before B arrived. Every response still applies its data.
        for (part, n) in [
            (reply(old, 1, 1, 2), 1),
            (reply(current, 2, 0, 2), 2),
            (reply(old, 1, 0, 2), 3),
        ] {
            core.on_state(sender, None, vec![contact(n)], Vec::new(), part);
        }
        assert!(core.devices.awaiting_state);
        let replies = &core.devices.state_replies[&sender];
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].id, MsgId([2; 16]));
        assert_eq!(replies[0].parts, BTreeSet::from([0]));
        for n in 1..=3 {
            assert!(core.peers.contains_key(&PeerId([n; 32])));
        }

        core.on_state(
            sender,
            None,
            vec![contact(4)],
            Vec::new(),
            reply(current, 2, 1, 2),
        );
        assert!(!core.devices.awaiting_state);
        assert!(core.devices.state_replies.is_empty());

        core.on_state(
            sender,
            None,
            vec![contact(5)],
            Vec::new(),
            reply(old, 1, 0, 2),
        );
        assert!(!core.devices.awaiting_state);
        assert!(core.devices.state_replies.is_empty());
        assert!(core.peers.contains_key(&PeerId([5; 32])));
    }

    #[test]
    fn a_current_response_can_finish_last_part_first_with_duplicates() {
        let mut core = requested_core(0);
        let sender = Addr::new(core.me.peer_id, cypher_types::DeviceId::FIRST);
        let request = current_request(&core);
        for index in [2, 0, 2] {
            core.on_state(
                sender,
                None,
                Vec::new(),
                Vec::new(),
                reply(request, 1, index, 3),
            );
            assert!(core.devices.awaiting_state);
        }
        core.on_state(
            sender,
            None,
            Vec::new(),
            Vec::new(),
            reply(request, 1, 1, 3),
        );
        assert!(!core.devices.awaiting_state);
    }

    #[test]
    fn a_retry_supersedes_the_old_request_without_ordering_random_ids() {
        let mut core = requested_core(0);
        let sender = Addr::new(core.me.peer_id, cypher_types::DeviceId::FIRST);
        let first = current_request(&core);
        core.on_state(sender, None, Vec::new(), Vec::new(), reply(first, 1, 0, 2));

        core.now = STATE_ASK_MS;
        core.ask_siblings_again();
        let second = current_request(&core);
        assert_ne!(first, second);
        assert!(core.devices.state_replies.is_empty());

        core.on_state(sender, None, Vec::new(), Vec::new(), reply(first, 1, 1, 2));
        assert!(core.devices.awaiting_state);
        assert!(core.devices.state_replies.is_empty());

        core.on_state(sender, None, Vec::new(), Vec::new(), reply(second, 2, 1, 2));
        core.on_state(sender, None, Vec::new(), Vec::new(), reply(second, 2, 0, 2));
        assert!(!core.devices.awaiting_state);
    }

    #[test]
    fn uncorrelated_responses_apply_but_do_not_complete_a_new_request() {
        let mut core = requested_core(0);
        let sender = Addr::new(core.me.peer_id, cypher_types::DeviceId::FIRST);
        core.on_state(sender, None, vec![contact(1)], Vec::new(), None);
        core.on_state(sender, None, vec![contact(2)], Vec::new(), part(1, 0, 1));
        assert!(core.devices.awaiting_state);
        assert!(core.devices.state_replies.is_empty());
        assert!(core.peers.contains_key(&PeerId([1; 32])));
        assert!(core.peers.contains_key(&PeerId([2; 32])));

        let request = current_request(&core);
        core.on_state(
            sender,
            None,
            Vec::new(),
            Vec::new(),
            reply(request, 2, 0, 1),
        );
        assert!(!core.devices.awaiting_state);

        let mut legacy = waiting_core(0);
        legacy.on_state(sender, None, Vec::new(), Vec::new(), None);
        assert!(!legacy.devices.awaiting_state);
    }

    #[test]
    fn response_tracking_is_bounded_and_isolated_per_sender() {
        let mut core = requested_core(0);
        let first = Addr::new(core.me.peer_id, cypher_types::DeviceId::FIRST);
        let second = Addr::new(core.me.peer_id, cypher_types::DeviceId(3));
        let request = current_request(&core);
        for (sender, id) in [(first, 1), (first, 2), (second, 4)] {
            core.on_state(
                sender,
                None,
                Vec::new(),
                Vec::new(),
                reply(request, id, 0, 2),
            );
        }
        assert_eq!(core.devices.state_replies.len(), 2);

        // A conflicting total for one response ID does not add a part. A
        // third response ID cannot evict either of the first two.
        core.on_state(first, None, Vec::new(), Vec::new(), reply(request, 1, 1, 3));
        core.on_state(first, None, Vec::new(), Vec::new(), reply(request, 3, 1, 2));
        assert_eq!(core.devices.state_replies[&first].len(), MAX_STATE_REPLIES);
        assert_eq!(core.devices.state_replies[&first][0].parts.len(), 1);

        core.on_state(
            second,
            None,
            Vec::new(),
            Vec::new(),
            reply(request, 4, 1, 2),
        );
        assert!(!core.devices.awaiting_state);
        assert!(core.devices.state_replies.is_empty());
    }

    #[test]
    fn state_retries_follow_the_whole_schedule_and_restart_bounded() {
        let mut core = waiting_core(0);
        core.ask_siblings();
        assert_eq!(core.devices.state_asks, 1);
        let mut requests = outbox_state_requests(&core);
        assert_eq!(requests.len(), 1);

        for (expected, minute) in (2_u32..=MAX_STATE_ASKS).zip([1, 3, 7, 15, 31, 63]) {
            core.now = minute * STATE_ASK_MS - 1;
            core.ask_siblings_again();
            assert_eq!(core.devices.state_asks, expected - 1, "early at {minute}");
            assert_eq!(outbox_state_requests(&core), requests, "early at {minute}");

            core.now = minute * STATE_ASK_MS;
            core.ask_siblings_again();
            assert_eq!(core.devices.state_asks, expected, "due at {minute}");
            assert_due_state_request(&core, &mut requests, expected, minute);
        }

        let sender = Addr::new(core.me.peer_id, cypher_types::DeviceId::FIRST);
        let request = current_request(&core);
        core.on_state(
            sender,
            None,
            Vec::new(),
            Vec::new(),
            reply(request, 1, 0, 2),
        );
        core.now = 127 * STATE_ASK_MS - 1;
        core.ask_siblings_again();
        assert_eq!(core.devices.state_replies.len(), 1);
        assert_eq!(core.devices.state_asks, MAX_STATE_ASKS);
        assert_eq!(outbox_state_requests(&core), requests);
        core.now = 127 * STATE_ASK_MS;
        core.ask_siblings_again();
        assert!(core.devices.state_replies.is_empty());
        assert_eq!(core.devices.state_asks, MAX_STATE_ASKS);
        assert!(core.devices.awaiting_state);
        assert_eq!(outbox_state_requests(&core), requests);

        let mut restarted = waiting_core(core.now);
        restarted.ask_siblings();
        assert_eq!(restarted.devices.state_asks, 1);
        assert_eq!(restarted.devices.next_state_ask, 128 * STATE_ASK_MS);
        let after_restart = outbox_state_requests(&restarted);
        assert_eq!(after_restart.len(), 1);
        assert!(!requests.contains(&after_restart[0]));
    }

    #[test]
    fn state_replies_never_exceed_the_part_limit() {
        let max = MAX_STATE_PARTS as usize * MAX_STATE_CONTACTS;
        let contacts: Vec<_> = (0..max)
            .map(|n| contact(u8::try_from(n % 256).unwrap_or_default()))
            .collect();
        let bodies =
            state_bodies(None, &[], &contacts, MsgId([9; 16]), None).expect("the exact limit fits");
        assert_eq!(bodies.len(), MAX_STATE_PARTS as usize);
        assert!(bodies.iter().all(|body| match body {
            SyncBody::State {
                part: Some(part), ..
            } => part.valid(),
            _ => false,
        }));

        let mut overflow = contacts;
        overflow.push(contact(255));
        state_bodies(None, &[], &overflow, MsgId([9; 16]), None).unwrap_err();
    }

    #[test]
    #[cfg_attr(miri, ignore = "builds 51,201 peers; over 20 minutes under Miri")]
    fn oversized_state_replies_send_no_parts_and_leave_requester_waiting() {
        let mut core = requested_core(0);
        core.outbox.items.clear();
        core.journal.effects.clear();
        for n in 0..=MAX_STATE_PARTS as usize * MAX_STATE_CONTACTS {
            let mut peer = [0_u8; 32];
            peer[..8].copy_from_slice(&(n as u64).to_be_bytes());
            core.peers.insert(PeerId(peer), Peer::new([0; 32]));
        }
        let sender = Addr::new(core.me.peer_id, cypher_types::DeviceId::FIRST);
        core.on_sync(
            sender,
            SyncBody::StateRequest {
                request: Some(MsgId([8; 16])),
            },
        );

        assert!(
            core.outbox.items.is_empty(),
            "not even one State part goes out"
        );
        assert_eq!(core.journal.effects.len(), 1);
        assert!(matches!(
            core.journal.effects[0],
            Effect::Emit(Event::Warning {
                reason: FailReason::TooLarge
            })
        ));
        assert!(core.devices.awaiting_state);
    }
}
