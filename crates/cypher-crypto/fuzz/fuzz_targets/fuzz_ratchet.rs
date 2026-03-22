#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Need at least 32 (shared secret) + 32 (ratchet pubkey) + 4 (msg_no) + 1 (ciphertext)
    if data.len() < 69 {
        return;
    }
    let shared_secret_bytes: [u8; 32] = data[..32].try_into().unwrap();
    let ratchet_key_bytes: [u8; 32] = data[32..64].try_into().unwrap();
    let msg_no = u32::from_le_bytes(data[64..68].try_into().unwrap());
    let ciphertext = &data[68..];

    let shared_secret = cypher_crypto::SharedSecret(shared_secret_bytes);
    let ratchet_key = x25519_dalek::PublicKey::from(ratchet_key_bytes);
    let our_secret = x25519_dalek::StaticSecret::random_from_rng(rand::rngs::OsRng);

    // Create a ratchet state as receiver and try to decrypt arbitrary data.
    // This must never panic — only return errors.
    let mut state = cypher_crypto::RatchetState::init_receiver(&shared_secret, our_secret);
    let _ = state.decrypt(ciphertext, &ratchet_key, msg_no);
});
