#![no_main]
use std::sync::LazyLock;

use cypher_crypto::{InitHeader, PrekeyBundle, sealed};
use libfuzzer_sys::fuzz_target;
use x25519_dalek::StaticSecret;

static RECIPIENT: LazyLock<StaticSecret> = LazyLock::new(|| StaticSecret::from([7u8; 32]));

fuzz_target!(|data: &[u8]| {
    if let Ok(bundle) = PrekeyBundle::decode(data) {
        let _ = bundle.verify();
    }
    let _ = InitHeader::decode_prefix(data);
    let _ = sealed::open(&RECIPIENT, data);
});
