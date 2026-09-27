#![expect(
    clippy::unwrap_used,
    reason = "test helpers fail loudly on broken fixtures"
)]

use cypher_crypto::double_ratchet::MAX_SKIP;
use cypher_crypto::handshake::{self, InitHeader};
use cypher_crypto::prekey::SignedPreKey;
use cypher_crypto::{
    CryptoError, Header, IdentityKeyPair, IdentitySeed, OneTimePreKey, PrekeyBundle, Ratchet,
};
use proptest::prelude::*;
use rand::SeedableRng;
use rand::rngs::OsRng;
use rand_chacha::ChaCha20Rng;

struct Responder {
    ik: IdentityKeyPair,
    spk: SignedPreKey,
    opk: OneTimePreKey,
}

impl Responder {
    fn new() -> Self {
        Self {
            ik: IdentityKeyPair::generate(),
            spk: SignedPreKey::generate(1, &mut OsRng),
            opk: OneTimePreKey::generate(100, &mut OsRng),
        }
    }

    fn bundle(&self, with_opk: bool) -> PrekeyBundle {
        PrekeyBundle::new(&self.ik, &self.spk, with_opk.then_some(&self.opk))
    }

    fn accept(
        &self,
        initiator: &IdentityKeyPair,
        header: &InitHeader,
    ) -> Result<Ratchet, CryptoError> {
        let opk = header.opk_id.map(|_| &self.opk);
        handshake::respond(&self.ik, &self.spk, opk, &initiator.peer_id(), header)
    }
}

fn establish(with_opk: bool) -> (Ratchet, Ratchet) {
    let alice = IdentityKeyPair::generate();
    let bob = Responder::new();
    let (mut a, header) = handshake::initiate(&alice, &bob.bundle(with_opk), &mut OsRng).unwrap();
    let mut b = bob.accept(&alice, &header).unwrap();

    let (h, ct) = a.encrypt(b"hello", b"").unwrap();
    assert_eq!(b.decrypt(&h, &ct, b"", &mut OsRng).unwrap(), b"hello");
    (a, b)
}

fn send(from: &mut Ratchet, msg: &[u8]) -> (Header, Vec<u8>) {
    from.encrypt(msg, b"").unwrap()
}

fn recv(to: &mut Ratchet, m: &(Header, Vec<u8>)) -> Result<Vec<u8>, CryptoError> {
    to.decrypt(&m.0, &m.1, b"", &mut OsRng)
}

#[test]
fn handshake_with_and_without_opk_then_both_directions() {
    for with_opk in [false, true] {
        let (mut a, mut b) = establish(with_opk);
        for i in 0..5u8 {
            let m = send(&mut b, &[i]);
            assert_eq!(recv(&mut a, &m).unwrap(), [i]);
            let m = send(&mut a, &[i, i]);
            assert_eq!(recv(&mut b, &m).unwrap(), [i, i]);
        }
    }
}

#[test]
fn responder_cannot_send_before_first_message() {
    let alice = IdentityKeyPair::generate();
    let bob = Responder::new();
    let (_, header) = handshake::initiate(&alice, &bob.bundle(true), &mut OsRng).unwrap();
    let mut b = bob.accept(&alice, &header).unwrap();
    assert!(!b.can_send());
    assert_eq!(b.encrypt(b"x", b"").unwrap_err(), CryptoError::NotReady);
}

#[test]
fn forged_init_header_is_rejected() {
    let alice = IdentityKeyPair::generate();
    let mallory = IdentityKeyPair::generate();
    let bob = Responder::new();
    let (_, mut header) = handshake::initiate(&alice, &bob.bundle(false), &mut OsRng).unwrap();

    header.identity_dh = mallory.dh_public_key().to_bytes();
    assert_eq!(
        bob.accept(&alice, &header).unwrap_err(),
        CryptoError::Signature
    );

    let (_, header) = handshake::initiate(&alice, &bob.bundle(false), &mut OsRng).unwrap();
    assert_eq!(
        bob.accept(&mallory, &header).unwrap_err(),
        CryptoError::Signature
    );
}

#[test]
fn wrong_prekey_ids_are_rejected() {
    let alice = IdentityKeyPair::generate();
    let bob = Responder::new();
    let (_, header) = handshake::initiate(&alice, &bob.bundle(true), &mut OsRng).unwrap();
    let err = handshake::respond(&bob.ik, &bob.spk, None, &alice.peer_id(), &header).unwrap_err();
    assert_eq!(err, CryptoError::Malformed);

    let other_spk = SignedPreKey::generate(2, &mut OsRng);
    let err = handshake::respond(
        &bob.ik,
        &other_spk,
        Some(&bob.opk),
        &alice.peer_id(),
        &header,
    )
    .unwrap_err();
    assert_eq!(err, CryptoError::Malformed);
}

#[test]
fn init_header_roundtrip() {
    let alice = IdentityKeyPair::generate();
    let bob = Responder::new();
    for with_opk in [false, true] {
        let (_, header) = handshake::initiate(&alice, &bob.bundle(with_opk), &mut OsRng).unwrap();
        let mut buf = Vec::new();
        header.encode(&mut buf);
        buf.extend_from_slice(b"tail");
        let (decoded, rest) = InitHeader::decode_prefix(&buf).unwrap();
        assert_eq!(decoded, header);
        assert_eq!(rest, b"tail");
    }
}

#[test]
fn late_messages_from_previous_chain_survive_dh_step() {
    let (mut a, mut b) = establish(true);
    let late0 = send(&mut a, b"late-0");
    let late1 = send(&mut a, b"late-1");
    let on_time = send(&mut a, b"on-time");
    assert_eq!(recv(&mut b, &on_time).unwrap(), b"on-time");

    let reply = send(&mut b, b"reply");
    assert_eq!(recv(&mut a, &reply).unwrap(), b"reply");
    let next = send(&mut a, b"new-chain");
    assert_eq!(recv(&mut b, &next).unwrap(), b"new-chain");

    assert_eq!(recv(&mut b, &late1).unwrap(), b"late-1");
    assert_eq!(recv(&mut b, &late0).unwrap(), b"late-0");
}

#[test]
fn chain_skipped_only_via_previous_counter() {
    let (mut a, mut b) = establish(false);
    let lost = send(&mut a, b"sent-before-step");
    let reply = send(&mut b, b"reply");
    recv(&mut a, &reply).unwrap();
    let after = send(&mut a, b"after");
    assert_eq!(recv(&mut b, &after).unwrap(), b"after");
    assert_eq!(recv(&mut b, &lost).unwrap(), b"sent-before-step");
}

#[test]
fn tampering_does_not_desync_and_replay_fails() {
    let (mut a, mut b) = establish(false);
    let m = send(&mut a, b"payload");

    let mut bad = m.clone();
    bad.1[0] ^= 0xFF;
    assert_eq!(recv(&mut b, &bad), Err(CryptoError::Aead));
    let mut bad_header = m.clone();
    bad_header.0.pn += 1;
    assert_eq!(recv(&mut b, &bad_header), Err(CryptoError::Aead));

    assert_eq!(recv(&mut b, &m).unwrap(), b"payload");
    assert_eq!(recv(&mut b, &m), Err(CryptoError::Aead));
}

#[test]
fn extra_aad_is_authenticated() {
    let (mut a, mut b) = establish(false);
    let (h, ct) = a.encrypt(b"x", b"ctx-1").unwrap();
    assert_eq!(
        b.decrypt(&h, &ct, b"ctx-2", &mut OsRng),
        Err(CryptoError::Aead)
    );
    assert_eq!(b.decrypt(&h, &ct, b"ctx-1", &mut OsRng).unwrap(), b"x");
}

#[test]
fn forged_dh_step_with_low_order_key_is_rejected() {
    let (_, mut b) = establish(false);
    let forged = Header {
        dh: [0; 32],
        pn: 0,
        n: 0,
    };
    assert_eq!(
        b.decrypt(&forged, &[0; 32], b"", &mut OsRng),
        Err(CryptoError::NonContributory)
    );
}

#[test]
fn too_many_skipped_is_rejected_without_desync() {
    let (mut a, mut b) = establish(false);
    let mut last = None;
    for _ in 0..=MAX_SKIP + 1 {
        last = Some(send(&mut a, b"x"));
    }
    assert_eq!(
        recv(&mut b, &last.unwrap()),
        Err(CryptoError::TooManySkipped)
    );
}

#[test]
fn skipped_key_store_evicts_oldest() {
    let (mut a, mut b) = establish(false);
    let mut pending = Vec::new();
    for round in 0..3 {
        let first = send(&mut a, b"old");
        for _ in 0..MAX_SKIP - 1 {
            send(&mut a, b"skip");
        }
        let trigger = send(&mut a, b"trigger");
        recv(&mut b, &trigger).unwrap();
        pending.push((round, first));
    }
    assert_eq!(recv(&mut b, &pending[0].1), Err(CryptoError::Aead));
    assert_eq!(recv(&mut b, &pending[2].1).unwrap(), b"old");
}

#[test]
fn snapshot_restore_continues_without_key_reuse() {
    let (mut a, mut b) = establish(true);
    let pending = send(&mut a, b"in-flight");
    let _skip = send(&mut a, b"skipped");
    let delivered = send(&mut a, b"delivered");
    recv(&mut b, &delivered).unwrap();

    let blob_a = a.to_bytes();
    let blob_b = b.to_bytes();
    let mut a2 = Ratchet::from_bytes(&blob_a).unwrap();
    let mut b2 = Ratchet::from_bytes(&blob_b).unwrap();

    assert_eq!(recv(&mut b2, &pending).unwrap(), b"in-flight");
    let fresh = send(&mut a2, b"after-restore");
    assert_ne!(fresh.1, delivered.1);
    assert_eq!(recv(&mut b2, &fresh).unwrap(), b"after-restore");
    let back = send(&mut b2, b"back");
    assert_eq!(recv(&mut a2, &back).unwrap(), b"back");

    Ratchet::from_bytes(&blob_a[..blob_a.len() - 1]).unwrap_err();
    Ratchet::from_bytes(b"garbage").unwrap_err();
}

#[test]
fn deterministic_with_seeded_rng() {
    let run = || {
        let mut rng = ChaCha20Rng::seed_from_u64(7);
        let alice = IdentitySeed([1; 32]).derive_identity();
        let bob = IdentitySeed([2; 32]).derive_identity();
        let spk = SignedPreKey::generate(1, &mut rng);
        let opk = OneTimePreKey::generate(2, &mut rng);
        let bundle = PrekeyBundle::new(&bob, &spk, Some(&opk));
        let (mut a, header) = handshake::initiate(&alice, &bundle, &mut rng).unwrap();
        let (h, ct) = a.encrypt(b"vector", b"").unwrap();
        let mut out = Vec::new();
        header.encode(&mut out);
        out.extend_from_slice(&h.encode());
        out.extend_from_slice(&ct);
        out
    };
    assert_eq!(run(), run());
}

type InFlight = Vec<((Header, Vec<u8>), Vec<u8>)>;

#[derive(Clone, Debug)]
enum Op {
    Send { from_a: bool },
    Deliver { to_a: bool, pick: usize, dup: bool },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        any::<bool>().prop_map(|from_a| Op::Send { from_a }),
        (any::<bool>(), any::<usize>(), prop::bool::weighted(0.1))
            .prop_map(|(to_a, pick, dup)| Op::Deliver { to_a, pick, dup }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn arbitrary_schedules_decrypt_every_message_once(ops in prop::collection::vec(op(), 1..200)) {
        let (mut a, mut b) = establish(true);
        let mut to_a: InFlight = Vec::new();
        let mut to_b: InFlight = Vec::new();
        let mut counter = 0u32;

        for op in ops {
            match op {
                Op::Send { from_a } => {
                    counter += 1;
                    let body = counter.to_le_bytes().to_vec();
                    let (sender, queue) = if from_a { (&mut a, &mut to_b) } else { (&mut b, &mut to_a) };
                    queue.push((send(sender, &body), body));
                }
                Op::Deliver { to_a: deliver_to_a, pick, dup } => {
                    let (receiver, queue) = if deliver_to_a { (&mut a, &mut to_a) } else { (&mut b, &mut to_b) };
                    if queue.is_empty() {
                        continue;
                    }
                    let (msg, body) = queue.remove(pick % queue.len());
                    prop_assert_eq!(recv(receiver, &msg).unwrap(), body);
                    if dup {
                        prop_assert!(recv(receiver, &msg).is_err());
                    }
                }
            }
        }
        for (msg, body) in to_a.drain(..) {
            prop_assert_eq!(recv(&mut a, &msg).unwrap(), body);
        }
        for (msg, body) in to_b.drain(..) {
            prop_assert_eq!(recv(&mut b, &msg).unwrap(), body);
        }
    }
}
