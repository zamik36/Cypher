use std::collections::HashMap;

use bytes::{BufMut, Bytes, BytesMut};
use cypher_crypto::onion::{self, ReplyKey};
use cypher_crypto::onion_keys::KeySet;
use cypher_wire::relay::{CORR_LEN, HEADER_LEN, TAG_KEYS, TAG_ONION};
use rand_core::CryptoRngCore;

/// How long to wait for the driver's relay channel before falling back.
const RELAY_WAIT_MS: u64 = 10_000;
/// A key set is fetched again once it has less than this left to offer.
const KEY_SET_REFRESH_SECS: u64 = 24 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Readiness {
    Onion,
    Session,
    /// Not decided yet (bootstrap, relay connect or key set in progress):
    /// retry later.
    Wait,
    Unavailable,
}

#[derive(Debug, Clone, Copy)]
enum Bootstrap {
    Pending,
    NoRelay,
    Relay { since: u64 },
}

/// What a correlation id waits for.
enum Awaited {
    /// The reply to request `req_id`, sealed with `key`.
    Reply { key: ReplyKey, req_id: u32 },
    /// The server's onion key set.
    KeySet,
}

struct Waiting {
    awaited: Awaited,
    deadline: u64,
}

/// What came back through the relay.
pub(super) enum Opened {
    /// A plain server frame, which may answer request `.0` and no other.
    Reply(u32, Bytes),
    /// A key set signed by the server's key: requests can be sealed now.
    KeySet,
}

/// Routing for requests that must not be linked to the session identity.
pub(super) struct Anon {
    bootstrap: Bootstrap,
    /// The server's long-term key: it signs the onion keys.
    signing_key: Option<[u8; 32]>,
    key_set: Option<KeySet>,
    relay_up: bool,
    require_onion: bool,
    waiting: HashMap<u64, Waiting>,
    next_corr: u64,
}

impl Default for Anon {
    fn default() -> Self {
        Self {
            bootstrap: Bootstrap::Pending,
            signing_key: None,
            key_set: None,
            relay_up: false,
            require_onion: false,
            waiting: HashMap::new(),
            next_corr: 0,
        }
    }
}

impl Anon {
    /// The server named its relay and its signing key (`relay`), or has no
    /// relay. A key set signed with another key is no longer used.
    pub(super) fn on_bootstrap(&mut self, relay: Option<[u8; 32]>, now_ms: u64) {
        match relay {
            Some(signing_key) => {
                if self.signing_key != Some(signing_key) {
                    self.key_set = None;
                }
                self.signing_key = Some(signing_key);
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
            self.waiting.clear();
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
            _ if self.relay_up && self.key_at(now_ms).is_some() => Readiness::Onion,
            Bootstrap::NoRelay => fallback,
            Bootstrap::Relay { since } if now_ms.saturating_sub(since) >= RELAY_WAIT_MS => fallback,
            Bootstrap::Pending | Bootstrap::Relay { .. } => Readiness::Wait,
        }
    }

    fn key_at(&self, now_ms: u64) -> Option<&cypher_crypto::onion_keys::EpochKey> {
        self.key_set.as_ref()?.pick(now_ms / 1000)
    }

    /// Asks the relay for the server's key set when there is none, or it is
    /// running out, and none is on its way.
    pub(super) fn key_set_request(&mut self, now_ms: u64, deadline: u64) -> Option<Bytes> {
        let fresh = self.key_set.as_ref().is_some_and(|set| {
            set.valid_until().saturating_sub(now_ms / 1000) >= KEY_SET_REFRESH_SECS
        });
        let asked = self
            .waiting
            .values()
            .any(|w| matches!(w.awaited, Awaited::KeySet));
        if !self.relay_up || self.signing_key.is_none() || fresh || asked {
            return None;
        }
        let corr = self.await_reply(Awaited::KeySet, deadline);
        let mut out = BytesMut::with_capacity(HEADER_LEN);
        out.put_u64_le(corr);
        out.put_u8(TAG_KEYS);
        Some(out.freeze())
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
        let key = *self.key_at(now_ms)?;
        let (blob, reply) =
            onion::seal_request(key.epoch, &key.public, frame, now_ms / 1000, rng).ok()?;
        let corr = self.await_reply(Awaited::Reply { key: reply, req_id }, deadline);
        let mut out = BytesMut::with_capacity(HEADER_LEN + blob.len());
        out.put_u64_le(corr);
        out.put_u8(TAG_ONION);
        out.put_slice(&blob);
        Some(out.freeze())
    }

    fn await_reply(&mut self, awaited: Awaited, deadline: u64) -> u64 {
        self.next_corr = self.next_corr.wrapping_add(1);
        self.waiting
            .insert(self.next_corr, Waiting { awaited, deadline });
        self.next_corr
    }

    /// What the relay sent back: a reply to one request, or the key set,
    /// taken only if the server's key signed it.
    pub(super) fn open(&mut self, reply: &[u8]) -> Option<Opened> {
        let (corr, body) = reply.split_first_chunk::<CORR_LEN>()?;
        let waiting = self.waiting.remove(&u64::from_le_bytes(*corr))?;
        match waiting.awaited {
            Awaited::Reply { key, req_id } => {
                let frame = onion::open_response(&key, body).ok()?;
                Some(Opened::Reply(req_id, Bytes::from(frame)))
            }
            Awaited::KeySet => {
                let set = KeySet::verify(body, &self.signing_key?).ok()?;
                self.key_set = Some(set);
                Some(Opened::KeySet)
            }
        }
    }

    pub(super) fn expire(&mut self, now_ms: u64) {
        self.waiting.retain(|_, w| w.deadline > now_ms);
    }
}

#[cfg(test)]
mod tests {
    use cypher_crypto::IdentityKeyPair;
    use cypher_crypto::onion_keys::EpochKey;
    use rand_core::OsRng;
    use x25519_dalek::{PublicKey, StaticSecret};

    use super::*;

    /// The server's side: its signing key, onion secret and key set.
    struct Server {
        signer: IdentityKeyPair,
        secret: StaticSecret,
    }

    impl Server {
        fn new() -> Self {
            Self {
                signer: IdentityKeyPair::generate(),
                secret: StaticSecret::random_from_rng(OsRng),
            }
        }

        fn signing_key(&self) -> [u8; 32] {
            *self.signer.peer_id().as_bytes()
        }

        /// The key set as the relay hands it back for `asked`.
        fn key_set(&self, asked: &[u8], not_after: u64) -> Vec<u8> {
            let key = EpochKey {
                epoch: 3,
                public: PublicKey::from(&self.secret).to_bytes(),
                not_before: 0,
                not_after,
            };
            let set = KeySet::sign(&self.signer, 0, vec![key]).unwrap().encode();
            [&asked[..CORR_LEN], &set].concat()
        }

        /// Opens a request and seals `answer` for its reply.
        fn answer(&self, sealed: &[u8], answer: &[u8]) -> Vec<u8> {
            assert_eq!(sealed[CORR_LEN], TAG_ONION);
            let (epoch, blob) = onion::split_epoch(&sealed[HEADER_LEN..]).unwrap();
            assert_eq!(epoch, 3);
            let opened = onion::open_request(&self.secret, blob).unwrap();
            let mut out = sealed[..CORR_LEN].to_vec();
            out.extend(onion::seal_response(&opened.reply, answer, &mut OsRng));
            out
        }
    }

    /// A client with the relay up and the server's key set taken.
    fn ready(server: &Server) -> Anon {
        let mut anon = Anon::default();
        anon.on_bootstrap(Some(server.signing_key()), 0);
        anon.set_relay_up(true);
        let ask = anon.key_set_request(0, 5_000).unwrap();
        let set = server.key_set(&ask, 1_000_000);
        assert!(matches!(anon.open(&set), Some(Opened::KeySet)));
        assert_eq!(anon.readiness(0), Readiness::Onion);
        anon
    }

    #[test]
    fn a_reply_names_the_request_it_answers_once() {
        let server = Server::new();
        let mut anon = ready(&server);
        let sealed = anon.seal(7, b"frame", 1_000, 5_000, &mut OsRng).unwrap();
        let reply = server.answer(&sealed, b"answer");
        let Some(Opened::Reply(req_id, frame)) = anon.open(&reply) else {
            panic!("a reply");
        };
        assert_eq!((req_id, &frame[..]), (7, &b"answer"[..]));
        assert!(anon.open(&reply).is_none(), "answered once");
    }

    #[test]
    fn replies_after_the_deadline_or_the_relay_are_dropped() {
        let server = Server::new();
        let mut anon = ready(&server);
        let late = anon.seal(1, b"a", 0, 5_000, &mut OsRng).unwrap();
        let lost = anon.seal(2, b"b", 0, 50_000, &mut OsRng).unwrap();
        anon.expire(5_000);
        assert!(anon.open(&server.answer(&late, b"x")).is_none());
        anon.set_relay_up(false);
        assert!(anon.open(&server.answer(&lost, b"y")).is_none());
        assert!(anon.open(b"short").is_none());
    }

    #[test]
    fn only_a_key_set_the_server_signed_is_taken_and_kept_fresh() {
        let server = Server::new();
        let mut anon = Anon::default();
        anon.set_relay_up(true);
        assert!(
            anon.key_set_request(0, 5_000).is_none(),
            "no key to check it"
        );
        anon.on_bootstrap(Some(server.signing_key()), 0);
        let ask = anon.key_set_request(0, 5_000).unwrap();
        assert!(anon.key_set_request(0, 5_000).is_none(), "one at a time");

        let forger = Server::new();
        assert!(anon.open(&forger.key_set(&ask, 1_000_000)).is_none());
        assert_eq!(anon.readiness(0), Readiness::Wait);

        let ask = anon.key_set_request(0, 5_000).unwrap();
        assert!(anon.open(&server.key_set(&ask, 1_000_000)).is_some());
        assert!(anon.key_set_request(0, 5_000).is_none(), "fresh");
        let late_ms = (1_000_000 - KEY_SET_REFRESH_SECS + 1) * 1000;
        assert!(
            anon.key_set_request(late_ms, late_ms + 5_000).is_some(),
            "running out"
        );
        assert_ne!(
            anon.readiness(1_000_000 * 1000),
            Readiness::Onion,
            "expired"
        );

        // Another signing key: the set it did not sign is dropped.
        anon.on_bootstrap(Some(forger.signing_key()), 0);
        assert_eq!(anon.readiness(0), Readiness::Wait);
    }

    #[test]
    fn readiness_waits_for_the_relay_and_keys_then_falls_back() {
        let mut anon = Anon::default();
        assert_eq!(anon.readiness(0), Readiness::Wait);
        anon.on_bootstrap(Some([9; 32]), 0);
        anon.set_relay_up(true);
        assert_eq!(
            anon.readiness(RELAY_WAIT_MS - 1),
            Readiness::Wait,
            "no keys yet"
        );
        assert_eq!(anon.readiness(RELAY_WAIT_MS), Readiness::Session);
        anon.set_require_onion(true);
        assert_eq!(anon.readiness(RELAY_WAIT_MS), Readiness::Unavailable);
        anon.on_bootstrap(None, 0);
        assert_eq!(anon.readiness(0), Readiness::Unavailable);
    }
}
