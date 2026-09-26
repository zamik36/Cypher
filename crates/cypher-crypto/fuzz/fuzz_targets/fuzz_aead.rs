#![no_main]
use cypher_crypto::aead;
use cypher_crypto::chunk::{ChunkCipher, FileKey};
use cypher_types::FileId;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 48 {
        return;
    }
    let key: [u8; 32] = data[..32].try_into().unwrap();
    let nonce: [u8; 12] = data[32..44].try_into().unwrap();
    let index = u32::from_le_bytes(data[44..48].try_into().unwrap());
    let body = &data[48..];
    let _ = aead::open(&key, &nonce, b"", body);

    let cipher = ChunkCipher::new(&FileKey::from_bytes(key), FileId([0; 16]), 8);
    let _ = cipher.open(index, &mut body.to_vec());
});
