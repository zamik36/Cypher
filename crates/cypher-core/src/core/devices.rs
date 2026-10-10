//! Which devices an identity has: our own list, kept on the server, and our
//! contacts' lists, fetched from it and announced by the contacts.

use std::collections::HashMap;

use bytes::Bytes;
use cypher_crypto::DeviceList;
use cypher_types::{Addr, DeviceId, MAX_DEVICES, PeerId};
use cypher_wire::{ClientMsg, DeliveryStatus};
use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};

use super::outbox::OutboxItem;
use super::{Conn, Core, Pending};
use crate::api::{Effect, Event, FailReason};
use crate::envelope::{Body, Envelope};
use crate::link::{LinkOffer, seal_handover};
use crate::peer::Peer;
use crate::relay;
use crate::store::{META_DEVICES, StoreOp, Table, session_key};

/// How often a device makes sure it is still on its identity's list.
const OWN_CHECK_MS: u64 = 60 * 60_000;
/// Least time between two lookups of one contact's devices.
const REFRESH_INTERVAL_MS: u64 = 10 * 60_000;
/// Attempts to add ourselves to a list someone else keeps changing.
const MAX_CONFLICTS: u8 = 3;

/// Why a device list was asked for.
#[derive(Debug)]
pub(super) enum DevicesFor {
    /// Our own: are we still on it?
    Own,
    /// A contact's, which may have changed.
    Refresh,
    /// The host of an invite we are joining by.
    Join { link: String },
}

/// Our own devices as kept in `meta`.
#[derive(Serialize, Deserialize, Default)]
pub(crate) struct OwnDevices {
    list: Option<Vec<u8>>,
    /// This device has been on the list: should it be missing now, it was
    /// removed.
    joined: bool,
    /// The list version contacts were last told of.
    announced: u64,
    /// This device asked its siblings for what they know.
    asked: bool,
    /// Names of the identity's devices, as far as this one knows them.
    names: Vec<(u32, String)>,
}

impl crate::Record for OwnDevices {
    const VERSION: u8 = 1;
}

/// What a device knows of its identity's devices and its contacts' lookups.
#[derive(Default)]
pub(super) struct Devices {
    own: Option<DeviceList>,
    joined: bool,
    announced: u64,
    pub(super) asked: bool,
    names: Vec<(u32, String)>,
    last_check: Option<u64>,
    conflicts: u8,
    last_refresh: HashMap<PeerId, u64>,
}

impl Devices {
    pub(super) fn from_record(record: Option<OwnDevices>) -> Self {
        let record = record.unwrap_or_default();
        Self {
            own: record.list.and_then(|l| DeviceList::decode(&l).ok()),
            joined: record.joined,
            announced: record.announced,
            asked: record.asked,
            names: record.names,
            ..Self::default()
        }
    }

    fn to_record(&self) -> OwnDevices {
        OwnDevices {
            list: self.own.as_ref().map(DeviceList::encode),
            joined: self.joined,
            announced: self.announced,
            asked: self.asked,
            names: self.names.clone(),
        }
    }
}

impl<R: CryptoRngCore> Core<R> {
    /// The devices of `peer` a message goes to; for this identity, its
    /// other devices.
    pub(super) fn devices_of(&self, peer: PeerId) -> Vec<DeviceId> {
        if peer == self.peer_id {
            let own = self.devices.own.as_ref();
            return own.map_or_else(Vec::new, |l| {
                l.devices()
                    .iter()
                    .copied()
                    .filter(|d| *d != self.device)
                    .collect()
            });
        }
        self.peers
            .get(&peer)
            .map_or_else(|| vec![DeviceId::FIRST], Peer::device_ids)
    }

    /// Makes sure this device is on its identity's list, at most hourly: a
    /// new device adds itself, and one that finds itself removed stops.
    pub(super) fn check_own_devices(&mut self) {
        let recent = self
            .devices
            .last_check
            .is_some_and(|at| self.now.saturating_sub(at) < OWN_CHECK_MS);
        if recent || self.conn != Conn::Ready {
            return;
        }
        self.devices.last_check = Some(self.now);
        let peer = self.peer_id;
        self.request(
            ClientMsg::FetchDevices { peer },
            Pending::FetchDevices {
                peer,
                purpose: DevicesFor::Own,
            },
            false,
        );
    }

    /// One of our own devices wrote: if this device does not know it yet,
    /// the list grew and is looked up again now.
    pub(super) fn check_own_device(&mut self, device: DeviceId) {
        let known = self
            .devices
            .own
            .as_ref()
            .is_some_and(|l| l.contains(device));
        if !known {
            self.devices.last_check = None;
            self.check_own_devices();
        }
    }

    /// Looks up the devices of a contact, at most every ten minutes each.
    pub(super) fn refresh_devices(&mut self, peer: PeerId) {
        let recent = self
            .devices
            .last_refresh
            .get(&peer)
            .is_some_and(|at| self.now.saturating_sub(*at) < REFRESH_INTERVAL_MS);
        if recent || self.conn != Conn::Ready {
            return;
        }
        self.devices.last_refresh.insert(peer, self.now);
        self.request(
            ClientMsg::FetchDevices { peer },
            Pending::FetchDevices {
                peer,
                purpose: DevicesFor::Refresh,
            },
            false,
        );
    }

    /// A device list the server gave (`None`: it has none for `peer`).
    pub(super) fn on_devices(&mut self, peer: PeerId, purpose: DevicesFor, list: Option<&[u8]>) {
        let list = match list.map(DeviceList::decode) {
            Some(Ok(list)) if list.identity() == peer => Some(list),
            // Forged or broken: as good as none.
            Some(_) => return,
            None => None,
        };
        match purpose {
            DevicesFor::Own => self.on_own_devices(list),
            DevicesFor::Refresh => {
                if let Some(list) = list {
                    self.learn_devices(peer, list);
                }
            }
            DevicesFor::Join { link } => self.join_devices(&link, peer, list),
        }
    }

    fn on_own_devices(&mut self, list: Option<DeviceList>) {
        match list {
            Some(list) if list.contains(self.device) => self.adopt_own(list),
            Some(_) if self.devices.joined => self.on_removed(),
            Some(list) => match list.with(&self.identity, self.device) {
                Ok(next) => self.publish_own(next),
                // The list is full.
                Err(_) => self.emit(Event::Warning {
                    reason: FailReason::Rejected,
                }),
            },
            None => {
                // No list yet (or it expired): ours, with us on it.
                let known = self.devices.own.as_ref();
                let version = known.map_or(1, |l| l.version().saturating_add(1));
                let mut ids: Vec<DeviceId> =
                    known.map(|l| l.devices().to_vec()).unwrap_or_default();
                ids.push(self.device);
                if let Ok(list) = DeviceList::sign(&self.identity, version, ids) {
                    self.publish_own(list);
                }
            }
        }
    }

    fn publish_own(&mut self, list: DeviceList) {
        self.publish_listing(list, None);
    }

    fn publish_listing(&mut self, list: DeviceList, linked: Option<(DeviceId, String)>) {
        let encoded = Bytes::from(list.encode());
        self.request(
            ClientMsg::PublishDevices { list: encoded },
            Pending::PublishDevices { list, linked },
            false,
        );
    }

    /// The server took our list, with a device this one just linked.
    pub(super) fn on_own_published(
        &mut self,
        list: DeviceList,
        linked: Option<(DeviceId, String)>,
    ) {
        self.devices.conflicts = 0;
        if let Some((device, name)) = linked {
            self.devices.names.retain(|(d, _)| *d != device.0);
            self.devices.names.push((device.0, name.clone()));
            self.emit(Event::DeviceLinked {
                device: device.0,
                name,
            });
        }
        self.adopt_own(list);
    }

    /// Hands this identity to the new device whose offer the user scanned:
    /// sealed to the offer's key, straight to its throwaway session.
    pub(super) fn link_device(&mut self, text: &str) {
        let Some(offer) = LinkOffer::parse(text) else {
            return self.emit(Event::LinkFailed {
                reason: FailReason::InvalidLink,
            });
        };
        let room = self
            .devices
            .own
            .as_ref()
            .filter(|_| self.devices.joined)
            .map(|l| !l.contains(offer.device) && l.devices().len() < MAX_DEVICES);
        let reason = match room {
            _ if self.conn != Conn::Ready => Some(FailReason::Offline),
            None => Some(FailReason::Offline),
            Some(false) => Some(FailReason::Rejected),
            Some(true) => None,
        };
        let nickname = self.profile_name.clone().unwrap_or_default();
        let body = seal_handover(&offer, &self.seed, &nickname, &mut self.rng);
        let (None, Some(body)) = (reason, body) else {
            let reason = reason.unwrap_or(FailReason::ServerError);
            return self.emit(Event::LinkFailed { reason });
        };
        let req_id = self.alloc_req();
        let deadline = self.now + super::REQUEST_TIMEOUT_MS;
        let send = ClientMsg::Send {
            to: offer.temp,
            device: DeviceId::FIRST,
            want_ack: true,
            body: Bytes::from(body),
        };
        self.pending
            .insert(req_id, (Pending::LinkSend { offer }, deadline));
        self.transmit(req_id, send);
    }

    /// The new device has the identity: list it.
    pub(super) fn on_link_sent(&mut self, offer: LinkOffer, status: DeliveryStatus) {
        let next = match status {
            DeliveryStatus::Delivered => self
                .devices
                .own
                .as_ref()
                .and_then(|l| l.with(&self.identity, offer.device).ok()),
            DeliveryStatus::Offline | DeliveryStatus::Busy => None,
        };
        match next {
            Some(list) => self.publish_listing(list, Some((offer.device, offer.name))),
            None => self.emit(Event::LinkFailed {
                reason: FailReason::Offline,
            }),
        }
    }

    /// Takes another device of this identity off its list. It learns so
    /// when it next checks, and contacts when this device tells them.
    pub(super) fn unlink_device(&mut self, device: DeviceId) {
        let next = self
            .devices
            .own
            .as_ref()
            .filter(|l| device != self.device && l.contains(device))
            .and_then(|l| l.without(&self.identity, device).ok());
        match next {
            Some(list) => {
                self.devices.names.retain(|(d, _)| *d != device.0);
                self.publish_own(list);
            }
            None => self.emit(Event::LinkFailed {
                reason: FailReason::Rejected,
            }),
        }
    }

    /// Someone published a newer list meanwhile: look again.
    pub(super) fn on_own_conflict(&mut self) {
        self.devices.conflicts = self.devices.conflicts.saturating_add(1);
        if self.devices.conflicts <= MAX_CONFLICTS {
            self.devices.last_check = None;
            self.check_own_devices();
        }
    }

    fn adopt_own(&mut self, list: DeviceList) {
        let devices = list
            .devices()
            .iter()
            .map(|d| {
                let name = self.devices.names.iter().find(|(n, _)| *n == d.0);
                (d.0, name.map(|(_, name)| name.clone()).unwrap_or_default())
            })
            .collect();
        self.emit(Event::OwnDevices {
            this: self.device.0,
            devices,
        });
        self.devices.own = Some(list);
        self.devices.joined = true;
        self.persist_own_devices();
        self.forget_unlisted_own();
        self.announce_devices();
        self.ask_siblings();
    }

    /// Ends the sessions with our own devices the list no longer has, telling
    /// each over it first, so a removed device that is online stops now
    /// rather than at its next check.
    fn forget_unlisted_own(&mut self) {
        let Some(list) = &self.devices.own else {
            return;
        };
        let gone: Vec<Addr> = self
            .sessions
            .keys()
            .filter(|a| a.peer == self.peer_id && !list.contains(a.device))
            .copied()
            .collect();
        if gone.is_empty() {
            return;
        }
        let list = list.encode();
        let farewell = Envelope {
            msg_id: self.random_id(),
            sent_at_ms: self.now,
            body: Body::Devices {
                list,
                inboxes: Vec::new(),
            },
        }
        .encode();
        for addr in gone {
            self.say_farewell(addr, &farewell);
            self.sessions.remove(&addr);
            self.persist(StoreOp::Delete {
                table: Table::Sessions,
                key: session_key(addr),
            });
            self.drop_targets(addr);
        }
    }

    /// Hands a removed device the list without it, at most once and only if
    /// it is online: one away learns when it next checks.
    fn say_farewell(&mut self, addr: Addr, farewell: &[u8]) {
        if !self.is_ready() {
            return;
        }
        let Some(session) = self.sessions.get_mut(&addr) else {
            return;
        };
        if !session.ratchet.can_send() {
            return;
        }
        let Ok((header, ct)) = session.ratchet.encrypt(farewell, b"") else {
            return;
        };
        let body = relay::message_body(session.pending_init.as_ref(), &header, &ct);
        let req_id = self.alloc_req();
        self.transmit(
            req_id,
            ClientMsg::Send {
                to: addr.peer,
                device: addr.device,
                want_ack: false,
                body: Bytes::from(body),
            },
        );
    }

    /// A sibling says the list changed. It is signed, so it counts as much
    /// as the server's copy, as long as it is newer than the one known here.
    pub(super) fn on_own_list_body(&mut self, list: &[u8]) {
        let Ok(list) = DeviceList::decode(list) else {
            return;
        };
        let known = self.devices.own.as_ref().map_or(0, DeviceList::version);
        if self.devices.joined && list.identity() == self.peer_id && list.version() > known {
            self.on_own_devices(Some(list));
        }
    }

    /// This device was taken off its identity's list: it stops, and the
    /// user decides what happens to what it holds.
    fn on_removed(&mut self) {
        self.conn = Conn::Offline;
        self.emit(Event::DeviceUnlinked);
        self.effects.push(Effect::Disconnect { reconnect: false });
    }

    /// Tells every contact of a list version they have not heard of, with the
    /// inbox of each device so they can write to it offline at once.
    fn announce_devices(&mut self) {
        let Some(list) = &self.devices.own else {
            return;
        };
        if list.version() <= self.devices.announced {
            return;
        }
        let version = list.version();
        let inboxes: Vec<(u32, [u8; 32])> = list
            .devices()
            .iter()
            .map(|d| {
                (
                    d.0,
                    cypher_wire::inbox_id(&self.seed.derive_inbox_secret(*d)),
                )
            })
            .collect();
        let body = Body::Devices {
            list: list.encode(),
            inboxes,
        };
        let mut contacts: Vec<PeerId> = self
            .peers
            .iter()
            .filter(|(_, p)| !p.request && !p.blocked)
            .map(|(id, _)| *id)
            .collect();
        contacts.sort_unstable();
        for peer in contacts {
            self.send_control(peer, body.clone());
        }
        self.devices.announced = version;
        self.persist_own_devices();
    }

    pub(super) fn persist_own_devices(&mut self) {
        let record = self.devices.to_record();
        let op = self
            .vault
            .put(Table::Meta, META_DEVICES.to_vec(), &record, &mut self.rng);
        self.persist(op);
    }

    /// A contact announced its devices.
    pub(super) fn on_devices_body(
        &mut self,
        from: PeerId,
        list: &[u8],
        inboxes: Vec<(u32, [u8; 32])>,
    ) {
        let Ok(list) = DeviceList::decode(list) else {
            return;
        };
        if list.identity() != from {
            return;
        }
        let known = self.peers.get(&from).and_then(|p| p.devices.as_ref());
        if known.is_some_and(|k| k.version() > list.version()) {
            return;
        }
        let inboxes: Vec<(DeviceId, [u8; 32])> = inboxes
            .into_iter()
            .map(|(d, i)| (DeviceId(d), i))
            .filter(|(d, _)| list.contains(*d))
            .take(MAX_DEVICES)
            .collect();
        self.learn_devices(from, list);
        if let Some(p) = self.peers.get_mut(&from) {
            p.inboxes = inboxes;
            self.persist_peer(&from);
        }
    }

    /// A contact's list, unless it is older than one already seen: the
    /// server could hand out an old one to bring a removed device back.
    pub(super) fn learn_devices(&mut self, peer: PeerId, list: DeviceList) {
        let Some(p) = self.peers.get_mut(&peer) else {
            return;
        };
        if p.devices
            .as_ref()
            .is_some_and(|k| k.version() >= list.version())
        {
            return;
        }
        p.inboxes.retain(|(d, _)| list.contains(*d));
        p.devices = Some(list);
        self.persist_peer(&peer);
        self.forget_unlisted(peer);
    }

    /// Ends what this device has with any device of `peer` no longer listed:
    /// its session, what was on its way to it, file transfers with it.
    fn forget_unlisted(&mut self, peer: PeerId) {
        let Some(p) = self.peers.get(&peer) else {
            return;
        };
        let gone: Vec<Addr> = self
            .sessions
            .keys()
            .filter(|a| a.peer == peer && !p.lists(a.device))
            .copied()
            .collect();
        let unreached: Vec<DeviceId> = self
            .outbox
            .values()
            .filter(|i| i.peer == peer)
            .flat_map(OutboxItem::devices)
            .filter(|d| !p.lists(*d))
            .collect();
        for addr in gone {
            self.sessions.remove(&addr);
            self.persist(StoreOp::Delete {
                table: Table::Sessions,
                key: session_key(addr),
            });
            self.cancel_transfers_with_device(addr);
        }
        for device in unreached {
            self.drop_targets(Addr::new(peer, device));
        }
    }
}
