//! Keeping the devices of one identity alike: each tells the others what
//! the user did on it, and a new device asks them for what they know.

use cypher_crypto::DeviceList;
use cypher_types::{Addr, MsgId, PeerId};
use rand_core::CryptoRngCore;

use super::Core;
use crate::api::{Content, Event, MessageStatus, StoredMessage};
use crate::envelope::{Body, ContactState, MAX_STATE_CONTACTS, SyncBody};
use crate::peer::{Peer, clean_name};

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

    /// A device just on its identity's list, knowing no one yet, asks its
    /// siblings once for what they know.
    pub(super) fn ask_siblings(&mut self) {
        if self.devices.asked || !self.peers.is_empty() {
            return;
        }
        if self.devices_of(self.peer_id).is_empty() {
            return;
        }
        self.devices.asked = true;
        self.persist_own_devices();
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
            } => {
                if self.peers.contains_key(&peer) {
                    self.store_message(StoredMessage {
                        msg_id,
                        peer,
                        outgoing: true,
                        sent_at_ms,
                        status: MessageStatus::Sent,
                        content,
                    });
                }
            }
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
                self.remove_peer(&peer);
                self.emit(Event::ContactsChanged);
            }
            SyncBody::Profile { name } => {
                self.adopt_profile_name(name.as_deref());
            }
            SyncBody::Link { code, at_ms } => self.adopt_link(&code, at_ms),
            SyncBody::StateRequest => self.send_state(sender),
            SyncBody::State {
                profile,
                contacts,
                links,
            } => {
                if profile.is_some() {
                    self.adopt_profile_name(profile.as_deref());
                }
                for contact in contacts {
                    self.adopt_contact(contact);
                }
                for (code, at_ms) in links {
                    self.adopt_link(&code, at_ms);
                }
                self.emit(Event::ContactsChanged);
            }
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
        let mut parts: Vec<Vec<ContactState>> = contacts
            .chunks(MAX_STATE_CONTACTS)
            .map(<[ContactState]>::to_vec)
            .collect();
        if parts.is_empty() {
            parts.push(Vec::new());
        }
        for (n, contacts) in parts.into_iter().enumerate() {
            let first = n == 0;
            let state = SyncBody::State {
                profile: if first {
                    self.profile_name.clone()
                } else {
                    None
                },
                contacts,
                links: if first {
                    self.own_links.clone()
                } else {
                    Vec::new()
                },
            };
            self.send_control_to(to, Body::Sync(state));
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
