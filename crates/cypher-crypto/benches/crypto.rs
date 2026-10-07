//! Hot paths of every message and file: ratchet steps, chunk AEAD, sealed
//! sender, the X3DH handshake and onion requests.

#![expect(clippy::unwrap_used, reason = "benchmark fixtures fail loudly")]

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use cypher_crypto::handshake;
use cypher_crypto::prekey::SignedPreKey;
use cypher_crypto::{
    ChunkCipher, FileKey, IdentityKeyPair, OneTimePreKey, PrekeyBundle, Ratchet, onion, sealed,
};
use cypher_types::DeviceId;
use cypher_types::FileId;
use rand::rngs::OsRng;
use x25519_dalek::{PublicKey, StaticSecret};

const MESSAGE: &[u8] = &[0x42; 1024];
const FILE_CHUNK: usize = 256 * 1024;
const MEDIA_CHUNK: usize = 64 * 1024;

/// An established session: Alice's first message already reached Bob.
fn session() -> (Ratchet, Ratchet) {
    let alice = IdentityKeyPair::generate();
    let bob = IdentityKeyPair::generate();
    let spk = SignedPreKey::generate(1, &mut OsRng);
    let bundle = PrekeyBundle::new(&bob, DeviceId(2), &spk, None);
    let (mut a, header) = handshake::initiate(&alice, DeviceId(1), &bundle, &mut OsRng).unwrap();
    let mut b = handshake::respond(
        &bob,
        DeviceId(2),
        &spk,
        None,
        (&alice.peer_id(), DeviceId(1)),
        &header,
    )
    .unwrap();
    let (h, ct) = a.encrypt(b"hi", b"").unwrap();
    b.decrypt(&h, &ct, b"", &mut OsRng).unwrap();
    (a, b)
}

fn ratchet(c: &mut Criterion) {
    let mut group = c.benchmark_group("ratchet");
    group.throughput(Throughput::Bytes(MESSAGE.len() as u64));
    let (mut a, _) = session();
    group.bench_function("encrypt_1k", |bench| {
        bench.iter(|| a.encrypt(MESSAGE, b"").unwrap());
    });
    let (mut a3, mut b3) = session();
    group.bench_function("roundtrip_1k", |bench| {
        bench.iter(|| {
            let (h, ct) = a3.encrypt(MESSAGE, b"").unwrap();
            b3.decrypt(&h, &ct, b"", &mut OsRng).unwrap()
        });
    });
    group.finish();
}

fn chunks(c: &mut Criterion) {
    let cipher = ChunkCipher::new(&FileKey::random(&mut OsRng), FileId([7; 16]), 1_000);
    let mut group = c.benchmark_group("chunk");
    for (name, size) in [("file_256k", FILE_CHUNK), ("media_64k", MEDIA_CHUNK)] {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(format!("seal_{name}"), |bench| {
            bench.iter_batched_ref(
                || vec![0x5Au8; size],
                |buf| cipher.seal(3, buf).unwrap(),
                BatchSize::LargeInput,
            );
        });
        let mut sealed = vec![0x5Au8; size];
        cipher.seal(3, &mut sealed).unwrap();
        group.bench_function(format!("open_{name}"), |bench| {
            bench.iter_batched_ref(
                || sealed.clone(),
                |buf| cipher.open(3, buf).unwrap(),
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

fn sealed_sender(c: &mut Criterion) {
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = PublicKey::from(&secret).to_bytes();
    let blob = sealed::seal(&public, MESSAGE, &mut OsRng).unwrap();
    let mut group = c.benchmark_group("sealed");
    group.bench_function("seal_1k", |bench| {
        bench.iter(|| sealed::seal(&public, MESSAGE, &mut OsRng).unwrap());
    });
    group.bench_function("open_1k", |bench| {
        bench.iter(|| sealed::open(&secret, &blob).unwrap());
    });
    group.finish();
}

fn x3dh(c: &mut Criterion) {
    let alice = IdentityKeyPair::generate();
    let bob = IdentityKeyPair::generate();
    let spk = SignedPreKey::generate(1, &mut OsRng);
    let opk = OneTimePreKey::generate(9, &mut OsRng);
    let bundle = PrekeyBundle::new(&bob, DeviceId(2), &spk, Some(&opk));
    c.bench_function("x3dh/initiate_and_respond", |bench| {
        bench.iter(|| {
            let (_, header) =
                handshake::initiate(&alice, DeviceId(1), &bundle, &mut OsRng).unwrap();
            handshake::respond(
                &bob,
                DeviceId(2),
                &spk,
                Some(&opk),
                (&alice.peer_id(), DeviceId(1)),
                &header,
            )
            .unwrap()
        });
    });
}

fn onion_requests(c: &mut Criterion) {
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = PublicKey::from(&secret).to_bytes();
    let mut group = c.benchmark_group("onion");
    group.bench_function("seal_open_request_1k", |bench| {
        bench.iter(|| {
            let (blob, _) = onion::seal_request(&public, MESSAGE, 0, &mut OsRng).unwrap();
            onion::open_request(&secret, &blob).unwrap()
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    ratchet,
    chunks,
    sealed_sender,
    x3dh,
    onion_requests
);
criterion_main!(benches);
