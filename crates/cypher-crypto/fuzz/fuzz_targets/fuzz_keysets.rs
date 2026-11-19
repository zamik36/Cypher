//! Signed lists and onion replies: DeviceList::decode, KeySet::verify
//! against a pinned key, and both sides of the one-hop onion (the reply a
//! client opens and the request signaling opens), including what is parsed
//! after the AEAD succeeds. The first byte picks the decoder.
//!
//! Random bytes almost never carry a valid signature, so mode 5 signs a
//! list and a key set, then flips a byte and/or appends a tail: whatever
//! still decodes must encode back to exactly those bytes.
#![no_main]
use std::sync::LazyLock;

use cypher_crypto::onion::{self, ReplyKey};
use cypher_crypto::onion_keys::{EpochKey, KeySet};
use cypher_crypto::{DeviceList, IdentityKeyPair, IdentitySeed, aead, sealed};
use cypher_types::DeviceId;
use libfuzzer_sys::fuzz_target;
use rand::SeedableRng;
use rand::rngs::StdRng;
use x25519_dalek::{PublicKey, StaticSecret};

const REPLY_BYTES: [u8; 32] = [0x42; 32];
const RESPONSE_AAD: &[u8] = b"cypher/v2/onion-response";

fn signer() -> IdentityKeyPair {
    IdentityKeyPair::from_seed(&IdentitySeed([9; 32]))
}
fn pinned() -> [u8; 32] {
    *SIGNER.peer_id().as_bytes()
}
fn onion_secret() -> StaticSecret {
    StaticSecret::from([5u8; 32])
}
fn onion_pub() -> [u8; 32] {
    PublicKey::from(&*ONION).to_bytes()
}
/// A reply key with known bytes: only an opened request yields one.
fn reply() -> ReplyKey {
    let mut plain = REPLY_BYTES.to_vec();
    plain.extend_from_slice(&0u64.to_le_bytes());
    plain.extend_from_slice(&0u32.to_le_bytes());
    let mut rng = StdRng::seed_from_u64(1);
    let blob = sealed::seal(&ONION_PUB, &plain, &mut rng).expect("seal");
    onion::open_request(&ONION, &blob).expect("open").reply
}

static SIGNER: LazyLock<IdentityKeyPair> = LazyLock::new(signer);
static PINNED: LazyLock<[u8; 32]> = LazyLock::new(pinned);
static ONION: LazyLock<StaticSecret> = LazyLock::new(onion_secret);
static ONION_PUB: LazyLock<[u8; 32]> = LazyLock::new(onion_pub);
static REPLY: LazyLock<ReplyKey> = LazyLock::new(reply);

fn byte(d: &[u8], i: usize) -> u8 {
    d.get(i).copied().unwrap_or(0)
}

fn u64_at(d: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(std::array::from_fn(|k| byte(d, i + k)))
}

/// `encoded` with one byte flipped and/or a tail of `d` appended, as the
/// first bytes of `d` pick; neither when both pick bits are clear.
fn mutate(encoded: &[u8], d: &[u8]) -> Vec<u8> {
    let mut out = encoded.to_vec();
    let pick = byte(d, 0);
    if pick & 1 != 0 && !out.is_empty() {
        let at = usize::from(u16::from_le_bytes([byte(d, 1), byte(d, 2)])) % out.len();
        if let Some(b) = out.get_mut(at) {
            *b ^= byte(d, 3) | 1;
        }
    }
    if pick & 2 != 0 {
        let tail = d.get(4..).unwrap_or_default();
        out.extend(tail.iter().take(usize::from(pick >> 2)));
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    match mode % 6 {
        0 => {
            if let Ok(list) = DeviceList::decode(rest) {
                assert_eq!(list.encode(), rest, "device list not canonical");
            }
        }
        1 => {
            if let Ok(set) = KeySet::verify(rest, &PINNED) {
                assert_eq!(set.encode(), rest, "key set not canonical");
                let _ = set.pick(u64_at(rest, 0));
                let _ = set.valid_until();
            }
        }
        2 => {
            let _ = onion::open_response(&REPLY, rest);
        }
        3 => {
            // Arbitrary plaintext under the right key: the post-AEAD parser.
            let nonce = [3u8; aead::NONCE_LEN];
            let seal = |plain: &[u8]| {
                [
                    &nonce[..],
                    &aead::seal(&REPLY_BYTES, &nonce, RESPONSE_AAD, plain),
                ]
                .concat()
            };
            if let Ok(frame) = onion::open_response(&REPLY, &seal(rest)) {
                assert_eq!(Some(frame.as_slice()), rest.get(4..4 + frame.len()));
            }
            // Positive control: a well-formed plaintext sealed the same way
            // must open, or a drifted key or AAD would skip the case above.
            let len = u32::try_from(rest.len()).unwrap().to_le_bytes();
            let framed = [&len[..], rest].concat();
            let opened = onion::open_response(&REPLY, &seal(&framed));
            assert_eq!(
                opened.as_deref(),
                Ok(rest),
                "hand-sealed reply did not open"
            );
            let mut rng = StdRng::seed_from_u64(3);
            let sealed = onion::seal_response(&REPLY, rest, &mut rng);
            assert_eq!(onion::open_response(&REPLY, &sealed).as_deref(), Ok(rest));
        }
        4 => {
            // Arbitrary plaintext sealed to the onion key signaling holds.
            let mut rng = StdRng::seed_from_u64(2);
            if let Ok(blob) = sealed::seal(&ONION_PUB, rest, &mut rng) {
                if let Ok(opened) = onion::open_request(&ONION, &blob) {
                    assert!(opened.frame.len() <= rest.len());
                }
            }
            if let Ok((_, sealed)) = onion::split_epoch(rest) {
                let _ = onion::open_request(&ONION, sealed);
            }
        }
        _ => {
            // Whatever signs must decode back to itself.
            let devices = rest
                .chunks(4)
                .take(8)
                .map(|c| DeviceId(u32::from_le_bytes(std::array::from_fn(|k| byte(c, k)))));
            if let Ok(list) = DeviceList::sign(&SIGNER, u64_at(rest, 0), devices) {
                let encoded = list.encode();
                assert_eq!(DeviceList::decode(&encoded).as_ref(), Ok(&list));
                let mutated = mutate(&encoded, rest);
                if let Ok(back) = DeviceList::decode(&mutated) {
                    assert_eq!(back.encode(), mutated, "device list not canonical");
                }
            }
            let keys = rest
                .chunks(13)
                .take(5)
                .map(|c| EpochKey {
                    epoch: u32::from(byte(c, 0)),
                    public: [byte(c, 1); 32],
                    not_before: u64_at(c, 2) >> 8,
                    not_after: u64_at(c, 5),
                })
                .collect();
            if let Ok(set) = KeySet::sign(&SIGNER, u64_at(rest, 1), keys) {
                let encoded = set.encode();
                assert_eq!(KeySet::verify(&encoded, &PINNED).as_ref(), Ok(&set));
                let mutated = mutate(&encoded, rest.get(5..).unwrap_or_default());
                if let Ok(back) = KeySet::verify(&mutated, &PINNED) {
                    assert_eq!(back.encode(), mutated, "key set not canonical");
                }
            }
        }
    }
});
