#![expect(
    clippy::unwrap_used,
    reason = "test helpers fail loudly on broken fixtures"
)]

use std::collections::BTreeMap;

use cypher_crypto::double_ratchet::{MAX_DECRYPTED, MAX_SKIP, MAX_SKIPPED_AGE_MS, SkippedKey};
use cypher_crypto::handshake::{self, InitHeader};
use cypher_crypto::prekey::SignedPreKey;
use cypher_crypto::{
    CryptoError, Header, IdentityKeyPair, IdentitySeed, OneTimePreKey, PrekeyBundle, Ratchet,
};
use cypher_types::DeviceId;
use proptest::prelude::*;
use rand::SeedableRng;
use rand::rngs::OsRng;
use rand_chacha::ChaCha20Rng;

/// The initiator's device in these sessions.
const ALICE: DeviceId = DeviceId(1);
/// The responder's device.
const BOB: DeviceId = DeviceId(2);
/// The clock skipped keys are dated by.
const NOW: u64 = 1_000;

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
        PrekeyBundle::new(&self.ik, BOB, &self.spk, with_opk.then_some(&self.opk))
    }

    fn accept(
        &self,
        initiator: &IdentityKeyPair,
        header: &InitHeader,
    ) -> Result<Ratchet, CryptoError> {
        let opk = header.opk_id.map(|_| &self.opk);
        handshake::respond(
            &self.ik,
            BOB,
            &self.spk,
            opk,
            (&initiator.peer_id(), ALICE),
            header,
        )
    }
}

fn establish(with_opk: bool) -> (Ratchet, Ratchet) {
    let alice = IdentityKeyPair::generate();
    let bob = Responder::new();
    let (mut a, header) =
        handshake::initiate(&alice, ALICE, &bob.bundle(with_opk), &mut OsRng).unwrap();
    let mut b = bob.accept(&alice, &header).unwrap();

    let (h, ct) = a.encrypt(b"hello", b"").unwrap();
    assert_eq!(b.decrypt(&h, &ct, b"", NOW, &mut OsRng).unwrap(), b"hello");
    (a, b)
}

fn send(from: &mut Ratchet, msg: &[u8]) -> (Header, Vec<u8>) {
    from.encrypt(msg, b"").unwrap()
}

fn recv(to: &mut Ratchet, m: &(Header, Vec<u8>)) -> Result<Vec<u8>, CryptoError> {
    to.decrypt(&m.0, &m.1, b"", NOW, &mut OsRng)
}

/// What a session holds that a message could change: its snapshot, and
/// its skipped keys (their count).
fn state(r: &Ratchet) -> (Vec<u8>, String) {
    (r.to_bytes().to_vec(), format!("{r:?}"))
}

/// A session as a caller stores it: its snapshot, its skipped keys one by
/// one, and the digests of what it decrypted in [`MAX_DECRYPTED`] slots.
#[derive(Default)]
struct Stored {
    snapshot: Vec<u8>,
    keys: BTreeMap<u64, SkippedKey>,
    decrypted: BTreeMap<u64, (u64, [u8; 32])>,
}

impl Stored {
    fn of(r: &mut Ratchet) -> Self {
        let mut stored = Self::default();
        stored.save(r);
        stored
    }

    /// Stores what changed in `r`: new keys, then the snapshot, then the
    /// keys gone are deleted.
    fn save(&mut self, r: &mut Ratchet) {
        let changes = r.take_changes();
        for key in changes.stored {
            self.keys.insert(key.seq(), key);
        }
        self.snapshot = r.to_bytes().to_vec();
        for seq in changes.spent {
            self.keys.remove(&seq);
        }
        for (seq, digest) in changes.decrypted {
            self.decrypted
                .insert(seq % MAX_DECRYPTED as u64, (seq, digest));
        }
    }

    fn load(&self) -> Ratchet {
        let mut r = Ratchet::from_bytes(&self.snapshot, NOW).unwrap();
        for key in self.keys.values() {
            assert!(r.restore_skipped(key), "{key:?} is stored");
        }
        r.restore_decrypted(self.decrypted.values().copied().collect());
        r
    }
}

#[test]
#[cfg_attr(miri, ignore = "twenty DH ratchet steps; minutes under Miri")]
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
    let (_, header) = handshake::initiate(&alice, ALICE, &bob.bundle(true), &mut OsRng).unwrap();
    let mut b = bob.accept(&alice, &header).unwrap();
    assert!(!b.can_send());
    assert_eq!(b.encrypt(b"x", b"").unwrap_err(), CryptoError::NotReady);
}

#[test]
fn forged_init_header_is_rejected() {
    let alice = IdentityKeyPair::generate();
    let mallory = IdentityKeyPair::generate();
    let bob = Responder::new();
    let (_, mut header) =
        handshake::initiate(&alice, ALICE, &bob.bundle(false), &mut OsRng).unwrap();

    header.identity_dh = mallory.dh_public_key().to_bytes();
    assert_eq!(
        bob.accept(&alice, &header).unwrap_err(),
        CryptoError::Signature
    );

    let (_, header) = handshake::initiate(&alice, ALICE, &bob.bundle(false), &mut OsRng).unwrap();
    assert_eq!(
        bob.accept(&mallory, &header).unwrap_err(),
        CryptoError::Signature
    );
}

/// The devices a session joins are signed into its first message: a server
/// that relabels the sender's device, or delivers the message to another
/// device of the responder, gets it refused.
#[test]
fn relabelled_devices_are_rejected() {
    let alice = IdentityKeyPair::generate();
    let bob = Responder::new();
    let (_, header) = handshake::initiate(&alice, ALICE, &bob.bundle(false), &mut OsRng).unwrap();
    let as_device = |ours: DeviceId, theirs: DeviceId| {
        handshake::respond(
            &bob.ik,
            ours,
            &bob.spk,
            None,
            (&alice.peer_id(), theirs),
            &header,
        )
    };
    as_device(BOB, ALICE).unwrap();
    assert_eq!(
        as_device(BOB, DeviceId(9)).unwrap_err(),
        CryptoError::Signature
    );
    assert_eq!(
        as_device(DeviceId(9), ALICE).unwrap_err(),
        CryptoError::Signature
    );
}

#[test]
fn wrong_prekey_ids_are_rejected() {
    let alice = IdentityKeyPair::generate();
    let bob = Responder::new();
    let (_, header) = handshake::initiate(&alice, ALICE, &bob.bundle(true), &mut OsRng).unwrap();
    let err = handshake::respond(
        &bob.ik,
        BOB,
        &bob.spk,
        None,
        (&alice.peer_id(), ALICE),
        &header,
    )
    .unwrap_err();
    assert_eq!(err, CryptoError::Malformed);

    let other_spk = SignedPreKey::generate(2, &mut OsRng);
    let err = handshake::respond(
        &bob.ik,
        BOB,
        &other_spk,
        Some(&bob.opk),
        (&alice.peer_id(), ALICE),
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
        let (_, header) =
            handshake::initiate(&alice, ALICE, &bob.bundle(with_opk), &mut OsRng).unwrap();
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
fn tampering_does_not_desync_and_a_replay_is_a_duplicate() {
    let (mut a, mut b) = establish(false);
    let m = send(&mut a, b"payload");

    let mut bad = m.clone();
    bad.1[0] ^= 0xFF;
    assert_eq!(recv(&mut b, &bad), Err(CryptoError::Aead));
    let mut bad_header = m.clone();
    bad_header.0.pn += 1;
    assert_eq!(recv(&mut b, &bad_header), Err(CryptoError::Aead));

    assert_eq!(recv(&mut b, &m).unwrap(), b"payload");
    assert_eq!(recv(&mut b, &m), Err(CryptoError::Duplicate));
}

/// A message decrypted on each kind of path: in order, out of order from
/// the skipped store, on a chain since left behind, on the current chain.
/// A copy of any of them is a duplicate, and the session does not move.
#[test]
fn a_copy_of_a_decrypted_message_is_a_duplicate_and_changes_nothing() {
    let (mut a, mut b) = establish(true);
    let late = send(&mut a, b"late");
    let in_order = send(&mut a, b"in order");
    assert_eq!(recv(&mut b, &in_order).unwrap(), b"in order");
    assert_eq!(recv(&mut b, &late).unwrap(), b"late");
    let reply = send(&mut b, b"reply");
    assert_eq!(recv(&mut a, &reply).unwrap(), b"reply");
    let current = send(&mut a, b"current chain");
    assert_eq!(recv(&mut b, &current).unwrap(), b"current chain");

    for copy in [&late, &in_order, &current] {
        let before = state(&b);
        assert_eq!(recv(&mut b, copy), Err(CryptoError::Duplicate));
        assert_eq!(state(&b), before, "state untouched");
    }
    let next = send(&mut a, b"next");
    assert_eq!(recv(&mut b, &next).unwrap(), b"next");
    let back = send(&mut b, b"back");
    assert_eq!(recv(&mut a, &back).unwrap(), b"back");
}

/// A message altered in any part (a copy of one read, or one never read),
/// or read with other associated data, fails like any forgery, and no
/// failure moves the session: the message it stood for still decrypts.
#[test]
fn altered_messages_fail_and_change_nothing() {
    let (mut a, mut b) = establish(false);
    let read = send(&mut a, b"read");
    assert_eq!(recv(&mut b, &read).unwrap(), b"read");
    let fresh = send(&mut a, b"fresh");

    let mut body = read.clone();
    body.1[0] ^= 1;
    let mut tag = read.clone();
    *tag.1.last_mut().unwrap() ^= 1;
    let mut counter = read.clone();
    counter.0.pn += 1;
    let mut tampered = fresh.clone();
    tampered.1[0] ^= 1;
    let mut spent_position = fresh.clone();
    spent_position.0.n = read.0.n;
    let mut far_ahead = fresh.clone();
    far_ahead.0.n += MAX_SKIP + 1;
    let mut foreign_key = fresh.clone();
    foreign_key.0.dh = [9; 32];
    for (name, forged, error) in [
        ("altered body", &body, CryptoError::Aead),
        ("altered tag", &tag, CryptoError::Aead),
        ("altered header", &counter, CryptoError::Aead),
        ("tampered fresh message", &tampered, CryptoError::Aead),
        (
            "fresh at a spent position",
            &spent_position,
            CryptoError::Aead,
        ),
        ("far ahead", &far_ahead, CryptoError::TooManySkipped),
        ("unknown ratchet key", &foreign_key, CryptoError::Aead),
    ] {
        let before = state(&b);
        assert_eq!(recv(&mut b, forged), Err(error), "{name}");
        assert_eq!(state(&b), before, "{name}: state untouched");
    }
    let before = state(&b);
    assert_eq!(
        b.decrypt(&read.0, &read.1, b"other context", NOW, &mut OsRng),
        Err(CryptoError::Aead)
    );
    assert_eq!(state(&b), before, "other context: state untouched");

    assert_eq!(recv(&mut b, &fresh).unwrap(), b"fresh");
}

/// A session knows copies of the last [`MAX_DECRYPTED`] messages it
/// decrypted; a copy of an older one fails like any spent key.
#[test]
#[cfg_attr(miri, ignore = "hundreds of messages; minutes under Miri")]
fn copies_are_known_for_the_last_messages_decrypted() {
    let (mut a, mut b) = establish(false);
    let sent: Vec<_> = (0..=MAX_DECRYPTED).map(|_| send(&mut a, b"x")).collect();
    for m in &sent {
        recv(&mut b, m).unwrap();
    }
    assert_eq!(recv(&mut b, &sent[0]), Err(CryptoError::Aead), "forgotten");
    assert_eq!(recv(&mut b, &sent[1]), Err(CryptoError::Duplicate));
    assert_eq!(
        recv(&mut b, &sent[MAX_DECRYPTED]),
        Err(CryptoError::Duplicate)
    );
}

/// A session that replaces another takes over which messages the old one
/// decrypted: a copy of one is a duplicate there too, and changes nothing.
#[test]
fn a_new_session_knows_the_copies_of_the_one_it_replaces() {
    let (mut a, mut b) = establish(false);
    let read = send(&mut a, b"read in the old session");
    recv(&mut b, &read).unwrap();
    let (mut a2, mut b2) = establish(false);
    b2.inherit(&mut b);

    let before = state(&b2);
    assert_eq!(recv(&mut b2, &read), Err(CryptoError::Duplicate));
    assert_eq!(state(&b2), before, "state untouched");
    let fresh = send(&mut a2, b"new session");
    assert_eq!(recv(&mut b2, &fresh).unwrap(), b"new session");
}

/// Which messages were decrypted is stored a digest at a time, in as
/// many slots as are remembered: once restored, a copy of one of the last
/// [`MAX_DECRYPTED`] is still a duplicate, and numbering goes on, so the
/// next message takes the oldest slot.
#[test]
#[cfg_attr(miri, ignore = "hundreds of messages; minutes under Miri")]
fn a_restored_session_knows_its_copies() {
    let (mut a, mut b) = establish(false);
    let mut stored = Stored::of(&mut b);
    let sent: Vec<_> = (0..=MAX_DECRYPTED).map(|_| send(&mut a, b"x")).collect();
    for m in &sent {
        recv(&mut b, m).unwrap();
        let changes = b.take_changes();
        assert_eq!(changes.decrypted.len(), 1, "one digest per message");
        for (seq, digest) in changes.decrypted {
            stored
                .decrypted
                .insert(seq % MAX_DECRYPTED as u64, (seq, digest));
        }
        stored.snapshot = b.to_bytes().to_vec();
    }
    assert_eq!(stored.decrypted.len(), MAX_DECRYPTED);

    let mut restored = stored.load();
    assert_eq!(recv(&mut restored, &sent[1]), Err(CryptoError::Duplicate));
    assert_eq!(
        recv(&mut restored, sent.last().unwrap()),
        Err(CryptoError::Duplicate)
    );
    assert_eq!(
        recv(&mut restored, &sent[0]),
        Err(CryptoError::Aead),
        "forgotten"
    );
    let next = send(&mut a, b"next");
    recv(&mut restored, &next).unwrap();
    let digests = restored.take_changes().decrypted;
    assert_eq!(digests.len(), 1);
    let last = stored
        .decrypted
        .values()
        .map(|(seq, _)| *seq)
        .max()
        .unwrap();
    assert_eq!(digests[0].0, last + 1, "numbered on");
}

#[test]
fn extra_aad_is_authenticated() {
    let (mut a, mut b) = establish(false);
    let (h, ct) = a.encrypt(b"x", b"ctx-1").unwrap();
    assert_eq!(
        b.decrypt(&h, &ct, b"ctx-2", NOW, &mut OsRng),
        Err(CryptoError::Aead)
    );
    assert_eq!(b.decrypt(&h, &ct, b"ctx-1", NOW, &mut OsRng).unwrap(), b"x");
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
        b.decrypt(&forged, &[0; 32], b"", NOW, &mut OsRng),
        Err(CryptoError::NonContributory)
    );
}

#[test]
#[cfg_attr(miri, ignore = "derives thousands of skipped keys; minutes under Miri")]
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
#[cfg_attr(miri, ignore = "derives thousands of skipped keys; minutes under Miri")]
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
    let mut a2 = Ratchet::from_bytes(&blob_a, NOW).unwrap();
    let mut b2 = Stored::of(&mut b).load();

    assert_eq!(recv(&mut b2, &pending).unwrap(), b"in-flight");
    let fresh = send(&mut a2, b"after-restore");
    assert_ne!(fresh.1, delivered.1);
    assert_eq!(recv(&mut b2, &fresh).unwrap(), b"after-restore");
    let back = send(&mut b2, b"back");
    assert_eq!(recv(&mut a2, &back).unwrap(), b"back");

    Ratchet::from_bytes(&blob_a[..blob_a.len() - 1], NOW).unwrap_err();
    Ratchet::from_bytes(b"garbage", NOW).unwrap_err();
    Ratchet::from_bytes(b"", NOW).unwrap_err();
}

/// The snapshot does not grow with the keys a session keeps for messages
/// that have not come: they are stored one by one, each once, and only
/// what changed is stored again.
#[test]
#[cfg_attr(miri, ignore = "derives hundreds of skipped keys; minutes under Miri")]
fn skipped_keys_are_stored_apart_and_once() {
    let (mut a, mut b) = establish(false);
    let mut stored = Stored::of(&mut b);
    let empty = stored.snapshot.len();
    let late: Vec<_> = (0..300).map(|_| send(&mut a, b"late")).collect();
    recv(&mut b, &send(&mut a, b"on time")).unwrap();

    let changes = b.take_changes();
    assert_eq!(changes.stored.len(), late.len());
    assert!(changes.spent.is_empty());
    for key in changes.stored {
        stored.keys.insert(key.seq(), key);
    }
    stored.snapshot = b.to_bytes().to_vec();
    // The next key's number grows by a byte or two, the keys are not in it.
    assert!(
        stored.snapshot.len() <= empty + 2,
        "no keys in the snapshot"
    );

    recv(&mut b, &send(&mut a, b"next")).unwrap();
    let changes = b.take_changes();
    assert!(changes.stored.is_empty() && changes.spent.is_empty());

    assert_eq!(recv(&mut b, &late[7]).unwrap(), b"late");
    let changes = b.take_changes();
    assert!(changes.stored.is_empty());
    assert_eq!(changes.spent.len(), 1, "the key it took");
    stored.keys.remove(&changes.spent[0]);
    stored.snapshot = b.to_bytes().to_vec();

    let mut restored = stored.load();
    assert_eq!(
        recv(&mut restored, &late[7]),
        Err(CryptoError::Aead),
        "spent"
    );
    for (i, m) in late.iter().enumerate().filter(|(i, _)| *i != 7) {
        assert_eq!(recv(&mut restored, m).unwrap(), b"late", "{i}");
    }
}

/// Keys stored for a state whose snapshot was never stored (the app
/// stopped in between) are refused when the older snapshot is restored:
/// that state reads the same messages again, setting the keys aside anew.
#[test]
fn keys_stored_ahead_of_their_snapshot_are_refused() {
    let (mut a, mut b) = establish(false);
    let stored = Stored::of(&mut b);
    let late = send(&mut a, b"late");
    let on_time = send(&mut a, b"on time");
    recv(&mut b, &on_time).unwrap();
    let ahead = b.take_changes().stored;
    assert_eq!(ahead.len(), 1);

    let mut restored = stored.load();
    assert!(!restored.restore_skipped(&ahead[0]));
    assert_eq!(recv(&mut restored, &on_time).unwrap(), b"on time");
    assert_eq!(recv(&mut restored, &late).unwrap(), b"late");
}

/// A key set aside for a late message opens it until it has been kept
/// [`MAX_SKIPPED_AGE_MS`], and nothing from then on, restored or not:
/// expiring drops it, and it is to be deleted.
#[test]
fn skipped_keys_open_their_messages_for_a_limited_time() {
    let (mut a, mut b) = establish(false);
    let late = [send(&mut a, b"late 1"), send(&mut a, b"late 2")];
    recv(&mut b, &send(&mut a, b"on time")).unwrap();
    let stored = Stored::of(&mut b);
    let (last, expired) = (NOW + MAX_SKIPPED_AGE_MS - 1, NOW + MAX_SKIPPED_AGE_MS);
    let open =
        |r: &mut Ratchet, m: &(Header, Vec<u8>), at| r.decrypt(&m.0, &m.1, b"", at, &mut OsRng);

    assert!(!b.expire_skipped(last));
    assert_eq!(open(&mut b, &late[0], last).unwrap(), b"late 1");
    let before = state(&b);
    assert_eq!(open(&mut b, &late[1], expired), Err(CryptoError::Aead));
    assert_eq!(state(&b), before, "a failure changes nothing");
    assert!(b.expire_skipped(expired));
    assert!(!b.expire_skipped(expired), "nothing left to expire");
    assert_eq!(b.take_changes().spent.len(), 2, "both stored keys go");

    let mut restored = stored.load();
    assert_eq!(
        open(&mut restored, &late[1], expired),
        Err(CryptoError::Aead)
    );
    assert_eq!(open(&mut restored, &late[1], last).unwrap(), b"late 2");
}

/// A session that replaces another numbers its skipped keys on from the
/// old one's, and every key the old one stored is to be deleted; the new
/// session's keys still open its own late messages.
#[test]
fn a_new_session_numbers_its_keys_on_and_spends_the_old_ones() {
    let (mut a, mut b) = establish(false);
    let _late = send(&mut a, b"never comes");
    recv(&mut b, &send(&mut a, b"x")).unwrap();
    let old = Stored::of(&mut b);
    let old_seqs: Vec<u64> = old.keys.keys().copied().collect();

    let alice = IdentityKeyPair::generate();
    let bob = Responder::new();
    let (mut a2, header) =
        handshake::initiate(&alice, ALICE, &bob.bundle(false), &mut OsRng).unwrap();
    let mut b2 = bob.accept(&alice, &header).unwrap();
    let late = send(&mut a2, b"late in the new session");
    recv(&mut b2, &send(&mut a2, b"first")).unwrap();
    b2.inherit(&mut b);

    let changes = b2.take_changes();
    assert_eq!(changes.spent, old_seqs);
    assert_eq!(changes.stored.len(), 1);
    assert!(changes.stored[0].seq() > old_seqs[0]);
    assert_eq!(recv(&mut b2, &late).unwrap(), b"late in the new session");
}

#[test]
fn deterministic_with_seeded_rng() {
    let run = || {
        let mut rng = ChaCha20Rng::seed_from_u64(7);
        let alice = IdentitySeed([1; 32]).derive_identity();
        let bob = IdentitySeed([2; 32]).derive_identity();
        let spk = SignedPreKey::generate(1, &mut rng);
        let opk = OneTimePreKey::generate(2, &mut rng);
        let bundle = PrekeyBundle::new(&bob, BOB, &spk, Some(&opk));
        let (mut a, header) = handshake::initiate(&alice, ALICE, &bundle, &mut rng).unwrap();
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
    Send {
        from_a: bool,
    },
    Deliver {
        to_a: bool,
        pick: usize,
        dup: bool,
    },
    /// One side is stored and restored, as across an app restart.
    Restart {
        a: bool,
    },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        any::<bool>().prop_map(|from_a| Op::Send { from_a }),
        (any::<bool>(), any::<usize>(), prop::bool::weighted(0.1))
            .prop_map(|(to_a, pick, dup)| Op::Deliver { to_a, pick, dup }),
        any::<bool>().prop_map(|a| Op::Restart { a }),
    ]
}

/// Proptest settings; under Miri a handful of cases and no failure files,
/// which Miri's isolation forbids. Coverage of inputs is the fuzzers' job.
fn proptest_config(cases: u32) -> ProptestConfig {
    if cfg!(miri) {
        ProptestConfig {
            cases: 4,
            failure_persistence: None,
            ..ProptestConfig::default()
        }
    } else {
        ProptestConfig::with_cases(cases)
    }
}

proptest! {
    #![proptest_config(proptest_config(64))]

    #[test]
    #[cfg_attr(miri, ignore = "up to 200 ops per case; tens of minutes under Miri")]
    fn arbitrary_schedules_decrypt_every_message_once(ops in prop::collection::vec(op(), 1..200)) {
        let (mut a, mut b) = establish(true);
        let mut stored = [Stored::of(&mut a), Stored::of(&mut b)];
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
                        prop_assert_eq!(recv(receiver, &msg), Err(CryptoError::Duplicate));
                    }
                }
                Op::Restart { a: restart_a } => {
                    let (r, s) = if restart_a { (&mut a, &mut stored[0]) } else { (&mut b, &mut stored[1]) };
                    s.save(r);
                    *r = s.load();
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
