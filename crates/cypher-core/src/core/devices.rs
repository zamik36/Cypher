//! Which devices an identity has: our own list, kept on the server, and our
//! contacts' lists, fetched from it and announced by the contacts.

use std::collections::HashMap;

use cypher_crypto::DeviceList;
use cypher_types::{Addr, DeviceId, MAX_DEVICES, PeerId};
use cypher_wire::ClientMsg;
use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};

use super::outbox::OutboxItem;
use super::{Conn, Core, Pending};
use crate::api::{Effect, Event};
use crate::envelope::Body;
use crate::peer::Peer;
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
            ..Self::default()
        }
    }

    fn to_record(&self) -> OwnDevices {
        OwnDevices {
            list: self.own.as_ref().map(DeviceList::encode),
            joined: self.joined,
            announced: self.announced,
            asked: self.asked,
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
                    reason: crate::api::FailReason::Rejected,
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
        let encoded = bytes::Bytes::from(list.encode());
        self.request(
            ClientMsg::PublishDevices { list: encoded },
            Pending::PublishDevices { list },
            false,
        );
    }

    /// The server took our list.
    pub(super) fn on_own_published(&mut self, list: DeviceList) {
        self.devices.conflicts = 0;
        self.adopt_own(list);
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
        self.devices.own = Some(list);
        self.devices.joined = true;
        self.persist_own_devices();
        self.forget_unlisted_own();
        self.announce_devices();
        self.ask_siblings();
    }

    /// Ends the sessions with our own devices the list no longer has.
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
        for addr in gone {
            self.sessions.remove(&addr);
            self.persist(StoreOp::Delete {
                table: Table::Sessions,
                key: session_key(addr),
            });
            self.drop_targets(addr);
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
