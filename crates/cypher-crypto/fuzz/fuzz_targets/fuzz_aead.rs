#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 32 {
        return;
    }
    let key: [u8; 32] = data[..32].try_into().unwrap();
    let rest = &data[32..];
    let _ = cypher_crypto::aead::aead_decrypt(&key, b"fuzz", rest, b"");
});
