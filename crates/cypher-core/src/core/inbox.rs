//! This device's blind inboxes: one per epoch, read with requests signed by
//! that epoch's key, so the server never holds anything that reads them.
//!
//! Contacts hear of the inboxes in each `Hello` ([`InboxAd`]: this epoch's
//! and the next). When the epoch turns, every contact is greeted again with
//! the new ones; an inbox is read for three epochs and then closed, so a
//! contact still writing to it hears `NotFound` instead of losing the item.

use bytes::Bytes;
use cypher_crypto::inbox::{InboxKey, InboxOp, inbox_epoch};
use cypher_types::{Addr, DeviceId, PeerId};
use cypher_wire::{ClientMsg, InboxProof};
use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};

use super::anon::Readiness;
use super::{Conn, Core, Pending};
use crate::envelope::InboxAd;
use crate::store::{META_INBOX, Table};

/// How often the inboxes of the epoch before last and of the next one are
/// read too: only clocks well off from ours leave items there.
const INBOX_SWEEP_MS: u64 = 60 * 60_000;
/// Epochs after which an inbox surely holds nothing: it takes items for two
/// weeks after it was last read (two epochs after its own) and keeps them
/// two weeks more.
const CLOSE_HORIZON: u32 = 7;

/// How far this device's inboxes have turned, kept in `meta`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct InboxEpochs {
    /// The epoch whose inboxes contacts were last told of; none before
    /// this device ever told any.
    announced: Option<u32>,
    /// The inboxes of every epoch before this one are closed, or were never
    /// announced.
    open_from: u32,
}

impl crate::Record for InboxEpochs {
    const VERSION: u8 = 1;
}

impl<R: CryptoRngCore> Core<R> {
    /// The inbox epoch now.
    pub(super) fn inbox_epoch(&self) -> u32 {
        inbox_epoch(self.now / 1000)
    }

    fn inbox_key(&self, epoch: u32) -> InboxKey {
        self.seed.derive_inbox_key(self.device, epoch)
    }

    /// This device's inbox for the current epoch.
    pub fn inbox_id(&self) -> [u8; 32] {
        self.inbox_key(self.inbox_epoch()).id()
    }

    /// The inbox of the identity's `device` for the current epoch.
    pub(super) fn own_inbox_of(&self, device: DeviceId) -> [u8; 32] {
        self.seed.derive_inbox_key(device, self.inbox_epoch()).id()
    }

    /// The inboxes the identity's `device` reads, as contacts are told.
    pub(super) fn own_ad(&self, device: DeviceId) -> InboxAd {
        let epoch = self.inbox_epoch();
        let id = |epoch| self.seed.derive_inbox_key(device, epoch).id();
        InboxAd {
            epoch,
            ids: [id(epoch), id(epoch.saturating_add(1))],
        }
    }

    /// Proof that this device asks `op` of the inbox of `epoch` now.
    pub(super) fn inbox_proof(&self, epoch: u32, op: InboxOp, extra: &[u8]) -> InboxProof {
        let key = self.inbox_key(epoch);
        let timestamp = self.now / 1000;
        InboxProof {
            inbox: key.id(),
            timestamp,
            signature: key.sign_op(op, timestamp, extra),
        }
    }

    /// Reads the inboxes contacts may still be writing to: the current
    /// epoch's and the last one's, and now and then the ones beyond. One
    /// request per epoch is in flight at most.
    pub(super) fn fetch_inbox(&mut self) {
        if self.conn != Conn::Ready
            || !matches!(
                self.anon.readiness(self.now),
                Readiness::Onion | Readiness::Session
            )
        {
            return;
        }
        self.last_inbox_fetch = self.now;
        let now = self.now;
        let sweep = self
            .last_inbox_sweep
            .is_none_or(|at| now.saturating_sub(at) >= INBOX_SWEEP_MS);
        if sweep {
            self.last_inbox_sweep = Some(now);
        }
        let current = self.inbox_epoch();
        let window = [
            Some(current),
            current.checked_sub(1),
            current.checked_sub(2).filter(|_| sweep),
            current.checked_add(1).filter(|_| sweep),
        ];
        for epoch in window.into_iter().flatten() {
            if !self.inbox_busy(epoch) {
                let proof = self.inbox_proof(epoch, InboxOp::Fetch, &[]);
                self.request(
                    ClientMsg::InboxFetch {
                        proof,
                        close: false,
                    },
                    Pending::InboxFetch {
                        epoch,
                        close: false,
                    },
                    true,
                );
            }
        }
    }

    /// Whether a request on the inbox of `epoch` is on its way.
    fn inbox_busy(&self, epoch: u32) -> bool {
        self.pending.values().any(|(p, _)| {
            matches!(p, Pending::InboxFetch { epoch: e, .. } | Pending::InboxAck { epoch: e, .. }
                if *e == epoch)
        })
    }

    /// A batch from the inbox of `epoch`, from its last fetch if `close`.
    pub(super) fn on_inbox_batch(
        &mut self,
        epoch: u32,
        close: bool,
        claim: [u8; 16],
        items: Vec<Bytes>,
    ) {
        // The server claims nothing for an empty batch: no ack to send.
        if items.is_empty() {
            if close {
                self.on_inbox_closed(epoch);
            }
            return;
        }
        let full = items.len() >= cypher_wire::MAX_INBOX_BATCH;
        for item in items {
            self.on_inbox_item(&item);
        }
        let proof = self.inbox_proof(epoch, InboxOp::Ack, &claim);
        self.request(
            ClientMsg::InboxAck { proof, claim },
            Pending::InboxAck { epoch, close },
            true,
        );
        if full {
            self.last_inbox_fetch = 0;
        }
    }

    /// A device that never told of an inbox before: none older than now
    /// needs closing.
    pub(super) fn start_inbox_epochs(&mut self) {
        self.inbox_epochs.open_from = self.inbox_epoch();
        self.persist_inbox_epochs();
    }

    /// Once per epoch: tells every contact of the inboxes read from now
    /// on, then closes those no longer read, oldest first.
    pub(super) fn turn_inbox_epoch(&mut self) {
        if self.conn != Conn::Ready {
            return;
        }
        let current = self.inbox_epoch();
        if self.inbox_epochs.announced != Some(current) {
            self.inbox_epochs.announced = Some(current);
            self.persist_inbox_epochs();
            self.greet_again();
            self.push_step();
        }
        self.close_old_inbox();
    }

    /// Takes the last items from the oldest inbox still open beyond the
    /// three epochs read, and closes it: one request at a time, the next
    /// after it comes back empty.
    pub(super) fn close_old_inbox(&mut self) {
        let current = self.inbox_epoch();
        let floor = current.saturating_sub(CLOSE_HORIZON);
        if self.inbox_epochs.open_from < floor {
            self.inbox_epochs.open_from = floor;
            self.persist_inbox_epochs();
        }
        let epoch = self.inbox_epochs.open_from;
        if epoch.saturating_add(3) > current
            || self.conn != Conn::Ready
            || !matches!(
                self.anon.readiness(self.now),
                Readiness::Onion | Readiness::Session
            )
            || self.inbox_busy(epoch)
        {
            return;
        }
        let proof = self.inbox_proof(epoch, InboxOp::Close, &[]);
        self.request(
            ClientMsg::InboxFetch { proof, close: true },
            Pending::InboxFetch { epoch, close: true },
            true,
        );
    }

    fn on_inbox_closed(&mut self, epoch: u32) {
        if self.inbox_epochs.open_from == epoch {
            self.inbox_epochs.open_from = epoch.saturating_add(1);
            self.persist_inbox_epochs();
        }
        self.close_old_inbox();
    }

    fn persist_inbox_epochs(&mut self) {
        let op = self.vault.put(
            Table::Meta,
            META_INBOX.to_vec(),
            &self.inbox_epochs,
            &mut self.rng,
        );
        self.persist(op);
    }

    fn on_inbox_item(&mut self, item: &[u8]) {
        let Ok(plain) = cypher_crypto::sealed::open(&self.identity.dh_secret, item) else {
            return;
        };
        let Some((from, rest)) = plain.split_first_chunk::<32>() else {
            return;
        };
        let Some((device, body)) = rest.split_first_chunk::<4>() else {
            return;
        };
        let device = DeviceId(u32::from_le_bytes(*device));
        if !device.is_valid() {
            return;
        }
        let from = Addr::new(PeerId(*from), device);
        self.on_relay(from, &Bytes::copy_from_slice(body), true);
    }
}
