//! Push registration: the server learns "inbox X has news → wake this push
//! address", proven by the inbox key's signature. It only ever goes through the
//! onion relay, never the session, so the address is not tied to `PeerId`.
//!
//! The subscription follows the inboxes contacts are told of: this epoch's
//! and the next, registered again when the epoch turns. An inbox's
//! registration ends when it is closed.

use cypher_crypto::inbox::{InboxOp, push_extra};
use cypher_wire::{ClientMsg, ServerMsg};
use rand_core::CryptoRngCore;

use super::anon::Readiness;
use super::{Conn, Core, Pending};
use crate::api::{Event, FailReason};

/// A device's Web Push subscription (RFC 8030/8291).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Subscription {
    pub endpoint: String,
    pub p256dh: [u8; 65],
    pub auth: [u8; 16],
}

/// What the user asked for and how far the server got. In memory only: the
/// platform hands the subscription over again on every start.
#[derive(Debug, Default)]
pub(crate) struct PushState {
    wanted: bool,
    /// The server should forget us once the channel allows it.
    unregister: bool,
    key: Option<[u8; 65]>,
    sub: Option<Subscription>,
    progress: Progress,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Progress {
    #[default]
    Idle,
    AskingKey,
    /// Sent or confirmed for the inboxes of `epoch` and the next;
    /// registered again when the channel comes back or the epoch turns.
    Registered {
        epoch: u32,
    },
}

impl<R: CryptoRngCore> Core<R> {
    /// Asks for the server's push key; the UI subscribes with it.
    pub(super) fn enable_push(&mut self) {
        self.push.wanted = true;
        self.push.unregister = false;
        if let Some(key) = self.push.key {
            self.emit(Event::PushKey { key: key.to_vec() });
        }
        self.push_step();
    }

    pub(super) fn register_push(&mut self, sub: Subscription) {
        self.push.wanted = true;
        self.push.unregister = false;
        if self.push.sub.as_ref() != Some(&sub) {
            self.push.progress = Progress::Idle;
        }
        self.push.sub = Some(sub);
        self.push_step();
    }

    pub(super) fn disable_push(&mut self) {
        self.push = PushState {
            key: self.push.key,
            unregister: true,
            ..PushState::default()
        };
        self.push_step();
    }

    /// The relay channel came (back) up: register again, which also keeps
    /// the server's copy from expiring.
    pub(super) fn renew_push(&mut self) {
        if matches!(self.push.progress, Progress::Registered { .. }) {
            self.push.progress = Progress::Idle;
        }
        self.push_step();
    }

    /// Takes the next step the onion channel allows now; the rest waits for
    /// it to come up.
    pub(super) fn push_step(&mut self) {
        if self.conn != Conn::Ready || self.anon.readiness(self.now) != Readiness::Onion {
            return;
        }
        let epoch = self.inbox_epoch();
        if self.push.unregister {
            self.push.unregister = false;
            // Every inbox still open may hold a registration.
            for registered in epoch.saturating_sub(2)..=epoch.saturating_add(1) {
                let proof = self.inbox_proof(registered, InboxOp::PushUnregister, &[]);
                self.request(
                    ClientMsg::PushUnregister { proof },
                    Pending::PushUnregister,
                    true,
                );
            }
        } else if self.push.wanted
            && self.push.key.is_none()
            && self.push.progress == Progress::Idle
        {
            self.push.progress = Progress::AskingKey;
            self.request(ClientMsg::PushKey, Pending::PushKey, true);
        } else if let Some(sub) = self.push.sub.clone().filter(|_| {
            matches!(self.push.progress, Progress::Idle)
                || matches!(self.push.progress, Progress::Registered { epoch: e } if e != epoch)
        }) {
            self.push.progress = Progress::Registered { epoch };
            let extra = push_extra(&sub.endpoint, &sub.p256dh, &sub.auth);
            for inbox_epoch in [epoch, epoch.saturating_add(1)] {
                let msg = ClientMsg::PushRegister {
                    proof: self.inbox_proof(inbox_epoch, InboxOp::PushRegister, &extra),
                    endpoint: sub.endpoint.clone(),
                    p256dh: sub.p256dh,
                    auth: sub.auth,
                };
                let pending = Pending::PushRegister { epoch: inbox_epoch };
                self.request(msg, pending, true);
            }
        }
    }

    pub(super) fn on_push_response(&mut self, pending: &Pending, msg: &ServerMsg) {
        match (pending, msg) {
            (Pending::PushKey, ServerMsg::PushKey { key }) => {
                self.push.progress = Progress::Idle;
                self.push.key = Some(*key);
                if self.push.wanted {
                    self.emit(Event::PushKey { key: key.to_vec() });
                }
                self.push_step();
            }
            // Told once per turn: for the current epoch's inbox.
            (Pending::PushRegister { epoch }, ServerMsg::Done)
                if self.push.progress == (Progress::Registered { epoch: *epoch }) =>
            {
                self.emit(Event::PushRegistered);
            }
            _ => {}
        }
    }

    /// The server could not take part: refusals end it, a lost channel only
    /// waits for the next one.
    pub(super) fn on_push_failed(&mut self, pending: &Pending, reason: FailReason) {
        self.push.progress = Progress::Idle;
        if matches!(reason, FailReason::ServerError | FailReason::NotFound)
            || matches!(pending, Pending::PushKey) && reason == FailReason::Offline
        {
            self.emit(Event::PushUnavailable);
        }
    }
}
