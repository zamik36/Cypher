//! Linking a new device to an identity, without the server learning that
//! anything but two short-lived sessions talked.
//!
//! The new device makes a throwaway identity, connects with it and shows an
//! offer: that identity, its key agreement key, the device id it will take
//! and a name for it. A device of the identity scans the offer and sends
//! the identity's seed and nickname, sealed to the throwaway key, straight
//! to the throwaway session; then it adds the new device to its identity's
//! list. Whoever holds the offer can only receive, never read what others
//! send, and the seed is checked against the identity that sent it.

use bytes::Bytes;
use cypher_crypto::{IdentityKeyPair, IdentitySeed, sealed};
use cypher_types::hex;
use cypher_types::{DeviceId, PeerId, SESSION_AUTH_CONTEXT};
use cypher_wire::{ClientMsg, Frame, PROTOCOL_VERSION, ServerMsg};
use rand_core::CryptoRngCore;
use zeroize::Zeroizing;

use crate::api::{Effect, Event, FailReason, Input};
use crate::peer::MAX_NAME_CHARS;

/// What an offer looks like in text (and in its QR code).
pub const OFFER_PREFIX: &str = "cypher-device:";
const OFFER_VERSION: u8 = 1;
const HANDOVER_VERSION: u8 = 1;
/// How long a new device waits to be linked.
pub const OFFER_TTL_MS: u64 = 5 * 60_000;
const PING_INTERVAL_MS: u64 = 20_000;

/// What a new device shows for one of the identity's devices to scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkOffer {
    /// The throwaway identity the new device waits on.
    pub temp: PeerId,
    /// Its key agreement key, which the hand-over is sealed to.
    pub temp_dh: [u8; 32],
    /// The id the new device takes among the identity's devices.
    pub device: DeviceId,
    /// What the user calls the new device.
    pub name: String,
}

impl LinkOffer {
    pub fn to_text(&self) -> String {
        let mut raw = Vec::with_capacity(70 + self.name.len());
        raw.push(OFFER_VERSION);
        raw.extend_from_slice(self.temp.as_bytes());
        raw.extend_from_slice(&self.temp_dh);
        raw.extend_from_slice(&self.device.0.to_le_bytes());
        raw.extend_from_slice(self.name.as_bytes());
        format!("{OFFER_PREFIX}{}", hex::encode(raw))
    }

    /// An offer from its text, wherever it sits in what was scanned or
    /// pasted.
    pub fn parse(text: &str) -> Option<Self> {
        let start = text.find(OFFER_PREFIX)? + OFFER_PREFIX.len();
        let encoded: String = text
            .get(start..)?
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        let raw = hex::decode(encoded).ok()?;
        let (&[version], rest) = raw.split_first_chunk::<1>()?;
        let (temp, rest) = rest.split_first_chunk::<32>()?;
        let (temp_dh, rest) = rest.split_first_chunk::<32>()?;
        let (device, name) = rest.split_first_chunk::<4>()?;
        let device = DeviceId(u32::from_le_bytes(*device));
        let name = String::from_utf8(name.to_vec()).ok()?;
        let valid =
            version == OFFER_VERSION && device.is_valid() && name.chars().count() <= MAX_NAME_CHARS;
        valid.then_some(Self {
            temp: PeerId(*temp),
            temp_dh: *temp_dh,
            device,
            name,
        })
    }
}

/// What a linked device gets: the identity and its place in it.
pub struct Linked {
    pub seed: IdentitySeed,
    pub device: DeviceId,
    pub nickname: String,
}

/// The identity's seed and nickname, sealed to the new device's offer.
pub(crate) fn seal_handover(
    offer: &LinkOffer,
    seed: &IdentitySeed,
    nickname: &str,
    rng: &mut impl CryptoRngCore,
) -> Option<Vec<u8>> {
    let mut plain = Zeroizing::new(Vec::with_capacity(33 + nickname.len()));
    plain.push(HANDOVER_VERSION);
    plain.extend_from_slice(seed.as_bytes());
    plain.extend_from_slice(nickname.as_bytes());
    sealed::seal(&offer.temp_dh, &plain, rng).ok()
}

/// The new device's side: a session on the throwaway identity that waits
/// for the hand-over. Sans-IO like [`crate::Core`]: the driver feeds it the
/// same inputs and carries out the same effects.
pub struct Provision {
    temp: IdentityKeyPair,
    offer: LinkOffer,
    expires_at: u64,
    ready: bool,
    last_ping: u64,
    linked: Option<Linked>,
}

impl Provision {
    /// A new device called `name`, waiting from `now_ms`.
    pub fn new(name: &str, now_ms: u64, rng: &mut impl CryptoRngCore) -> Self {
        let mut seed = [0u8; 32];
        rng.fill_bytes(&mut seed);
        let temp = IdentitySeed(seed).derive_identity();
        let name: String = name.chars().take(MAX_NAME_CHARS).collect();
        let offer = LinkOffer {
            temp: temp.peer_id(),
            temp_dh: temp.dh_public_key().to_bytes(),
            device: DeviceId::random(rng),
            name: name.trim().to_owned(),
        };
        Self {
            temp,
            offer,
            expires_at: now_ms.saturating_add(OFFER_TTL_MS),
            ready: false,
            last_ping: now_ms,
            linked: None,
        }
    }

    pub fn offer(&self) -> &LinkOffer {
        &self.offer
    }

    /// The identity, once a device of it handed it over; asked for after
    /// [`Event::LinkedHere`].
    pub fn take_linked(&mut self) -> Option<Linked> {
        self.linked.take()
    }

    pub fn handle(&mut self, input: Input, now_ms: u64) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.linked.is_some() {
            return effects;
        }
        if now_ms >= self.expires_at {
            self.expires_at = u64::MAX;
            effects.push(Effect::Emit(Event::LinkFailed {
                reason: FailReason::Timeout,
            }));
            effects.push(Effect::Disconnect { reconnect: false });
            return effects;
        }
        match input {
            Input::Connected => {
                self.ready = false;
                let hello = ClientMsg::Hello {
                    version: PROTOCOL_VERSION,
                    peer: self.temp.peer_id(),
                    device: DeviceId::FIRST,
                };
                effects.push(Effect::Transmit(Frame::new(0, hello).encode()));
            }
            Input::Frame(raw) => self.on_frame(raw, &mut effects),
            Input::Tick
                if self.ready && now_ms.saturating_sub(self.last_ping) >= PING_INTERVAL_MS =>
            {
                self.last_ping = now_ms;
                effects.push(Effect::Transmit(Frame::new(0, ClientMsg::Ping).encode()));
            }
            Input::Disconnected
            | Input::Tick
            | Input::AnonymousFrame(_)
            | Input::AnonymousChannel { .. }
            | Input::Command(_)
            | Input::ChunkRead { .. }
            | Input::ChunkUnavailable { .. } => {}
        }
        effects
    }

    fn on_frame(&mut self, raw: Bytes, effects: &mut Vec<Effect>) {
        let Ok(Frame { msg, .. }) = Frame::<ServerMsg>::decode(raw) else {
            return;
        };
        match msg {
            ServerMsg::Challenge { nonce } => {
                let signed = [
                    SESSION_AUTH_CONTEXT,
                    nonce.as_slice(),
                    &DeviceId::FIRST.0.to_le_bytes(),
                ]
                .concat();
                let signature = self.temp.sign(&signed).to_bytes();
                let auth = ClientMsg::Auth { signature };
                effects.push(Effect::Transmit(Frame::new(0, auth).encode()));
            }
            ServerMsg::Ready => self.ready = true,
            ServerMsg::Recv { from, body, .. } => {
                if let Some(linked) = self.open_handover(from, &body) {
                    self.linked = Some(linked);
                    effects.push(Effect::Emit(Event::LinkedHere));
                    effects.push(Effect::Disconnect { reconnect: false });
                }
            }
            ServerMsg::Pong
            | ServerMsg::SendAck { .. }
            | ServerMsg::Keys { .. }
            | ServerMsg::KeysAck { .. }
            | ServerMsg::Devices { .. }
            | ServerMsg::LinkCreated { .. }
            | ServerMsg::LinkResolved { .. }
            | ServerMsg::InboxBatch { .. }
            | ServerMsg::Done
            | ServerMsg::BootstrapInfo { .. }
            | ServerMsg::PushKey { .. }
            | ServerMsg::Error { .. }
            | ServerMsg::Superseded => {}
        }
    }

    /// The hand-over, if it opens with our key and carries the seed of the
    /// very identity that sent it.
    fn open_handover(&self, from: PeerId, body: &[u8]) -> Option<Linked> {
        let plain = Zeroizing::new(sealed::open(&self.temp.dh_secret, body).ok()?);
        let (&[version], rest) = plain.split_first_chunk::<1>()?;
        let (seed, nickname) = rest.split_first_chunk::<32>()?;
        let seed = IdentitySeed(*seed);
        let genuine = version == HANDOVER_VERSION && seed.derive_identity().peer_id() == from;
        let nickname = String::from_utf8(nickname.to_vec()).ok()?;
        genuine.then(|| Linked {
            seed,
            device: self.offer.device,
            nickname,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn offers_round_trip_through_text_and_refuse_garbage() {
        let p = Provision::new("  Laptop  ", 0, &mut OsRng);
        let text = p.offer().to_text();
        assert!(text.starts_with(OFFER_PREFIX));
        let back = LinkOffer::parse(&format!("scanned: {text} trailing")).unwrap();
        assert_eq!(&back, p.offer());
        assert_eq!(back.name, "Laptop");
        assert!(LinkOffer::parse("cypher-device:zz").is_none());
        assert!(LinkOffer::parse("hello").is_none());
        let mut zero = p.offer().clone();
        zero.device = DeviceId(0);
        assert!(LinkOffer::parse(&zero.to_text()).is_none());
    }

    #[test]
    fn a_handover_from_another_identity_is_refused() {
        let p = Provision::new("Phone", 0, &mut OsRng);
        let seed = IdentitySeed::generate();
        let sealed = seal_handover(p.offer(), &seed, "alice", &mut OsRng).unwrap();
        let owner = seed.derive_identity().peer_id();
        let linked = p.open_handover(owner, &sealed).unwrap();
        assert_eq!(linked.seed.as_bytes(), seed.as_bytes());
        assert_eq!(
            (linked.device, linked.nickname.as_str()),
            (p.offer().device, "alice")
        );
        let stranger = IdentityKeyPair::generate().peer_id();
        assert!(p.open_handover(stranger, &sealed).is_none());
        assert!(p.open_handover(owner, b"not sealed").is_none());
    }

    #[test]
    fn an_offer_nobody_takes_expires() {
        let mut p = Provision::new("Tablet", 0, &mut OsRng);
        assert!(!p.handle(Input::Connected, 0).is_empty());
        let effects = p.handle(Input::Tick, OFFER_TTL_MS);
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::Emit(Event::LinkFailed {
                reason: FailReason::Timeout
            })
        )));
        assert!(p.take_linked().is_none());
    }
}
