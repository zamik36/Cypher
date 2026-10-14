use std::collections::HashMap;

use bytes::{BufMut, Bytes, BytesMut};
use cypher_crypto::onion::{self, ReplyKey};
use rand_core::CryptoRngCore;

/// Correlation id prefix on every anonymous frame; the relay echoes it.
pub(super) const CORR_LEN: usize = 8;

/// How long to wait for the driver's relay channel before falling back.
const RELAY_WAIT_MS: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Readiness {
    Onion,
    Session,
    /// Not decided yet (bootstrap or relay connect in progress): retry later.
    Wait,
    Unavailable,
}

#[derive(Debug, Clone, Copy)]
enum Bootstrap {
    Pending,
    NoRelay,
    Relay { since: u64 },
}

/// Routing for requests that must not be linked to the session identity.
pub(super) struct Anon {
    bootstrap: Bootstrap,
    onion_key: Option<[u8; 32]>,
    relay_up: bool,
    require_onion: bool,
    /// Per correlation id: the key its reply is sealed with, until when it
    /// is awaited, and the request it answers.
    replies: HashMap<u64, Reply>,
    next_corr: u64,
}

struct Reply {
    key: ReplyKey,
    deadline: u64,
    req_id: u32,
}

impl Default for Anon {
    fn default() -> Self {
        Self {
            bootstrap: Bootstrap::Pending,
            onion_key: None,
            relay_up: false,
            require_onion: false,
            replies: HashMap::new(),
            next_corr: 0,
        }
    }
}

impl Anon {
    pub(super) fn on_bootstrap(&mut self, relay: Option<[u8; 32]>, now_ms: u64) {
        match relay {
            Some(key) => {
                self.onion_key = Some(key);
                self.bootstrap = Bootstrap::Relay { since: now_ms };
            }
            None => self.bootstrap = Bootstrap::NoRelay,
        }
    }

    pub(super) fn on_disconnected(&mut self) {
        self.bootstrap = Bootstrap::Pending;
    }

    pub(super) fn set_relay_up(&mut self, up: bool) {
        self.relay_up = up;
        if !up {
            self.replies.clear();
        }
    }

    pub(super) fn set_require_onion(&mut self, require: bool) {
        self.require_onion = require;
    }

    pub(super) fn readiness(&self, now_ms: u64) -> Readiness {
        let fallback = if self.require_onion {
            Readiness::Unavailable
        } else {
            Readiness::Session
        };
        match self.bootstrap {
            _ if self.relay_up && self.onion_key.is_some() => Readiness::Onion,
            Bootstrap::NoRelay => fallback,
            Bootstrap::Relay { since } if now_ms.saturating_sub(since) >= RELAY_WAIT_MS => fallback,
            Bootstrap::Pending | Bootstrap::Relay { .. } => Readiness::Wait,
        }
    }

    /// Seals `frame`, request `req_id`, for the relay; only valid when
    /// readiness is `Onion`.
    pub(super) fn seal(
        &mut self,
        req_id: u32,
        frame: &[u8],
        now_ms: u64,
        deadline: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Option<Bytes> {
        let key = self.onion_key?;
        let (blob, reply) = onion::seal_request(&key, frame, now_ms / 1000, rng).ok()?;
        self.next_corr = self.next_corr.wrapping_add(1);
        let corr = self.next_corr;
        let reply = Reply {
            key: reply,
            deadline,
            req_id,
        };
        self.replies.insert(corr, reply);
        let mut out = BytesMut::with_capacity(CORR_LEN + blob.len());
        out.put_u64_le(corr);
        out.put_slice(&blob);
        Some(out.freeze())
    }

    /// Decrypts a relay reply back into a plain server frame, with the
    /// request it may answer: no other.
    pub(super) fn open(&mut self, reply: &[u8]) -> Option<(u32, Bytes)> {
        let (corr, blob) = reply.split_first_chunk::<CORR_LEN>()?;
        let reply = self.replies.remove(&u64::from_le_bytes(*corr))?;
        let frame = onion::open_response(&reply.key, blob).ok()?;
        Some((reply.req_id, Bytes::from(frame)))
    }

    pub(super) fn expire(&mut self, now_ms: u64) {
        self.replies.retain(|_, r| r.deadline > now_ms);
    }
}

#[cfg(test)]
mod tests {
    use rand_core::OsRng;
    use x25519_dalek::{PublicKey, StaticSecret};

    use super::*;

    fn relay() -> (Anon, StaticSecret) {
        let secret = StaticSecret::random_from_rng(OsRng);
        let mut anon = Anon::default();
        anon.on_bootstrap(Some(PublicKey::from(&secret).to_bytes()), 0);
        anon.set_relay_up(true);
        (anon, secret)
    }

    /// The relay's side: opens a request and seals `answer` for its reply.
    fn answer(secret: &StaticSecret, sealed: &[u8], answer: &[u8]) -> Vec<u8> {
        let (corr, blob) = sealed.split_at(CORR_LEN);
        let opened = onion::open_request(secret, blob).unwrap();
        let mut out = corr.to_vec();
        out.extend(onion::seal_response(&opened.reply, answer, &mut OsRng));
        out
    }

    #[test]
    fn a_reply_names_the_request_it_answers_once() {
        let (mut anon, secret) = relay();
        let sealed = anon.seal(7, b"frame", 1_000, 5_000, &mut OsRng).unwrap();
        let reply = answer(&secret, &sealed, b"answer");
        let (req_id, frame) = anon.open(&reply).unwrap();
        assert_eq!((req_id, &frame[..]), (7, &b"answer"[..]));
        assert!(anon.open(&reply).is_none(), "answered once");
    }

    #[test]
    fn replies_after_the_deadline_or_the_relay_are_dropped() {
        let (mut anon, secret) = relay();
        let late = anon.seal(1, b"a", 0, 5_000, &mut OsRng).unwrap();
        let lost = anon.seal(2, b"b", 0, 50_000, &mut OsRng).unwrap();
        anon.expire(5_000);
        assert!(anon.open(&answer(&secret, &late, b"x")).is_none());
        anon.set_relay_up(false);
        assert!(anon.open(&answer(&secret, &lost, b"y")).is_none());
        assert!(anon.open(b"short").is_none());
    }

    #[test]
    fn readiness_waits_for_the_relay_then_falls_back() {
        let mut anon = Anon::default();
        assert_eq!(anon.readiness(0), Readiness::Wait);
        anon.on_bootstrap(Some([9; 32]), 0);
        assert_eq!(anon.readiness(RELAY_WAIT_MS - 1), Readiness::Wait);
        assert_eq!(anon.readiness(RELAY_WAIT_MS), Readiness::Session);
        anon.set_require_onion(true);
        assert_eq!(anon.readiness(RELAY_WAIT_MS), Readiness::Unavailable);
        anon.set_relay_up(true);
        assert_eq!(anon.readiness(RELAY_WAIT_MS), Readiness::Onion);
        anon.on_bootstrap(None, 0);
        anon.set_relay_up(false);
        assert_eq!(anon.readiness(0), Readiness::Unavailable);
    }
}
