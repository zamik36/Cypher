//! This device's blind inboxes: one per epoch, read with requests signed by
//! that epoch's key, so the server never holds anything that reads them.

use bytes::Bytes;
use cypher_crypto::inbox::{InboxKey, InboxOp, inbox_epoch};
use cypher_types::{Addr, DeviceId, PeerId};
use cypher_wire::{ClientMsg, InboxProof};
use rand_core::CryptoRngCore;

use super::anon::Readiness;
use super::{Conn, Core, Pending};

/// How often the inboxes of the epoch before last and of the next one are
/// read too: only clocks well off from ours leave items there.
const INBOX_SWEEP_MS: u64 = 60 * 60_000;

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
        let sweep = self.now.saturating_sub(self.last_inbox_sweep) >= INBOX_SWEEP_MS;
        if sweep {
            self.last_inbox_sweep = self.now;
        }
        let current = self.inbox_epoch();
        let window = [
            Some(current),
            current.checked_sub(1),
            current.checked_sub(2).filter(|_| sweep),
            current.checked_add(1).filter(|_| sweep),
        ];
        for epoch in window.into_iter().flatten() {
            let in_flight = self
                .pending
                .values()
                .any(|(p, _)| matches!(p, Pending::InboxFetch { epoch: e } if *e == epoch));
            if !in_flight {
                let proof = self.inbox_proof(epoch, InboxOp::Fetch, &[]);
                self.request(
                    ClientMsg::InboxFetch {
                        proof,
                        close: false,
                    },
                    Pending::InboxFetch { epoch },
                    true,
                );
            }
        }
    }

    pub(super) fn on_inbox_batch(&mut self, epoch: u32, claim: [u8; 16], items: Vec<Bytes>) {
        // The server claims nothing for an empty batch: no ack to send.
        if items.is_empty() {
            return;
        }
        let full = items.len() >= cypher_wire::MAX_INBOX_BATCH;
        for item in items {
            self.on_inbox_item(&item);
        }
        let proof = self.inbox_proof(epoch, InboxOp::Ack, &claim);
        self.request(
            ClientMsg::InboxAck { proof, claim },
            Pending::InboxAck,
            true,
        );
        if full {
            self.last_inbox_fetch = 0;
        }
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
