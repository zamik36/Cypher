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
    replies: HashMap<u64, (ReplyKey, u64)>,
    next_corr: u64,
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

    /// Seals `frame` for the relay; only valid when readiness is `Onion`.
    pub(super) fn seal(
        &mut self,
        frame: &[u8],
        now_ms: u64,
        deadline: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Option<Bytes> {
        let key = self.onion_key?;
        let (blob, reply) = onion::seal_request(&key, frame, now_ms / 1000, rng).ok()?;
        self.next_corr = self.next_corr.wrapping_add(1);
        let corr = self.next_corr;
        self.replies.insert(corr, (reply, deadline));
        let mut out = BytesMut::with_capacity(CORR_LEN + blob.len());
        out.put_u64_le(corr);
        out.put_slice(&blob);
        Some(out.freeze())
    }

    /// Decrypts a relay reply back into a plain server frame.
    pub(super) fn open(&mut self, reply: &[u8]) -> Option<Bytes> {
        let (corr, blob) = reply.split_first_chunk::<CORR_LEN>()?;
        let (key, _) = self.replies.remove(&u64::from_le_bytes(*corr))?;
        onion::open_response(&key, blob).ok().map(Bytes::from)
    }

    pub(super) fn expire(&mut self, now_ms: u64) {
        self.replies.retain(|_, (_, deadline)| *deadline > now_ms);
    }
}
