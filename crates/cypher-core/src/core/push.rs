//! Push registration: the server learns "inbox X has news → wake this push
//! address", proven by the inbox secret. It only ever goes through the
//! onion relay, never the session, so the address is not tied to `PeerId`.

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
    /// Sent or confirmed; registered again when the channel comes back.
    Registered,
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
        if self.push.progress == Progress::Registered {
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
        let secret = *self.inbox_secret;
        if self.push.unregister {
            self.push.unregister = false;
            self.request(
                ClientMsg::PushUnregister { secret },
                Pending::PushUnregister,
                true,
            );
        } else if self.push.wanted
            && self.push.key.is_none()
            && self.push.progress == Progress::Idle
        {
            self.push.progress = Progress::AskingKey;
            self.request(ClientMsg::PushKey, Pending::PushKey, true);
        } else if let Some(sub) = self
            .push
            .sub
            .clone()
            .filter(|_| self.push.progress == Progress::Idle)
        {
            self.push.progress = Progress::Registered;
            let msg = ClientMsg::PushRegister {
                secret,
                endpoint: sub.endpoint,
                p256dh: sub.p256dh,
                auth: sub.auth,
            };
            self.request(msg, Pending::PushRegister, true);
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
            (Pending::PushRegister, ServerMsg::Done) => self.emit(Event::PushRegistered),
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
