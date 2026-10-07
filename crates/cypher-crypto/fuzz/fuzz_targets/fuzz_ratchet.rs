#![no_main]
use std::sync::LazyLock;

use cypher_crypto::handshake;
use cypher_crypto::prekey::SignedPreKey;
use cypher_crypto::{Header, IdentityKeyPair, PrekeyBundle, Ratchet};
use cypher_types::DeviceId;
use libfuzzer_sys::fuzz_target;
use rand::rngs::OsRng;

static SESSION: LazyLock<Vec<u8>> = LazyLock::new(|| {
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
    let (h, ct) = a.encrypt(b"seed", b"").unwrap();
    b.decrypt(&h, &ct, b"", &mut OsRng).unwrap();
    b.to_bytes().to_vec()
});

fuzz_target!(|data: &[u8]| {
    let mut ratchet = Ratchet::from_bytes(&SESSION).unwrap();
    if let Ok(header) = Header::decode(data.get(..40).unwrap_or_default()) {
        let _ = ratchet.decrypt(&header, &data[40..], b"", &mut OsRng);
    }
    let _ = Ratchet::from_bytes(data);
});
