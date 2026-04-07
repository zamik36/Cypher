use cypher_common::{Error, Result};
use cypher_crypto::aead_decrypt;
use x25519_dalek::PublicKey as X25519PublicKey;

use super::encoder::{
    response_nonce_material, ONION_PREFIX, SUBTYPE_RELAY_REQUEST, SUBTYPE_RELAY_RESPONSE,
};

/// Extract the ephemeral public key, circuit_id, and seq_no from a raw onion request.
///
/// Wire format: `[0x02][32B pk][16B circuit_id][8B seq_no LE][ciphertext...]`
pub fn extract_request_header(data: &[u8]) -> Result<(X25519PublicKey, [u8; 16], u64)> {
    // 1 + 32 + 16 + 8 = 57 minimum
    if data.len() < 57 {
        return Err(Error::Protocol("onion request too short for header".into()));
    }
    if data[0] != ONION_PREFIX {
        return Err(Error::Protocol("missing onion prefix".into()));
    }
    let mut pk_bytes = [0u8; 32];
    pk_bytes.copy_from_slice(&data[1..33]);
    let mut circuit_id = [0u8; 16];
    circuit_id.copy_from_slice(&data[33..49]);
    let seq_no = u64::from_le_bytes(data[49..57].try_into().unwrap());
    Ok((X25519PublicKey::from(pk_bytes), circuit_id, seq_no))
}

/// Decode an incoming onion request on the **relay side**.
///
/// Expects wire format: `[0x02][32B ephemeral_pk][16B circuit_id][8B seq_no][AEAD ciphertext]`
///
/// The caller must supply `circuit_key` (derived from DH with the ephemeral
/// public key and the relay's static secret).
pub fn decode_relay_request(
    circuit_key: &[u8; 32],
    circuit_id: &[u8; 16],
    seq_no: u64,
    data: &[u8],
) -> Result<Vec<u8>> {
    // 1 + 32 + 16 + 8 = 57 minimum header
    if data.len() < 57 {
        return Err(Error::Protocol("onion request too short".into()));
    }
    if data[0] != ONION_PREFIX {
        return Err(Error::Protocol("missing onion prefix".into()));
    }

    let ciphertext = &data[57..]; // skip prefix + pk + circuit_id + seq_no

    // Build nonce material: circuit_id || seq_no
    let mut nonce_material = [0u8; 24];
    nonce_material[..16].copy_from_slice(circuit_id);
    nonce_material[16..].copy_from_slice(&seq_no.to_le_bytes());

    let inner = aead_decrypt(circuit_key, &nonce_material, ciphertext, circuit_id)?;

    if inner.is_empty() || inner[0] != SUBTYPE_RELAY_REQUEST {
        return Err(Error::Protocol("unexpected onion subtype".into()));
    }
    if inner.len() < 1 + 16 + 4 {
        return Err(Error::Protocol("onion inner too short".into()));
    }

    // Verify circuit_id matches
    let inner_circuit_id = &inner[1..17];
    if inner_circuit_id != circuit_id {
        return Err(Error::Protocol("circuit_id mismatch in onion".into()));
    }

    let payload_len = u32::from_le_bytes(inner[17..21].try_into().unwrap()) as usize;
    if 21 + payload_len > inner.len() {
        return Err(Error::Protocol("onion payload length exceeds inner".into()));
    }

    Ok(inner[21..21 + payload_len].to_vec())
}

/// Decode an onion relay response on the **client side**.
///
/// Expects wire format: `[0x02][8B seq_no LE][AEAD ciphertext]`
pub fn decode_relay_response(
    circuit_key: &[u8; 32],
    circuit_id: &[u8; 16],
    data: &[u8],
) -> Result<Vec<u8>> {
    // 1 + 8 = 9 minimum
    if data.len() < 9 || data[0] != ONION_PREFIX {
        return Err(Error::Protocol("missing onion prefix in response".into()));
    }

    let seq_no = u64::from_le_bytes(data[1..9].try_into().unwrap());
    let ciphertext = &data[9..];

    let nonce_material = response_nonce_material(circuit_id, seq_no);
    let inner = aead_decrypt(circuit_key, &nonce_material, ciphertext, circuit_id)?;

    if inner.is_empty() || inner[0] != SUBTYPE_RELAY_RESPONSE {
        return Err(Error::Protocol("unexpected onion response subtype".into()));
    }
    if inner.len() < 1 + 16 + 4 {
        return Err(Error::Protocol("onion response inner too short".into()));
    }

    let payload_len = u32::from_le_bytes(inner[17..21].try_into().unwrap()) as usize;
    if 21 + payload_len > inner.len() {
        return Err(Error::Protocol(
            "onion response payload length invalid".into(),
        ));
    }

    Ok(inner[21..21 + payload_len].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onion::circuit::Circuit;
    use crate::onion::encoder;
    use x25519_dalek::StaticSecret;

    #[test]
    fn request_roundtrip() {
        let relay_secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let relay_pk = X25519PublicKey::from(&relay_secret);
        let circuit = Circuit::new(&relay_pk);

        let payload = b"InboxFetch(inbox_42)";
        let encoded = encoder::encode_relay_request(&circuit, payload).unwrap();

        // Relay side: extract header, derive key, decode
        let (client_pk, cid, seq_no) = extract_request_header(&encoded).unwrap();
        assert_eq!(cid, circuit.circuit_id);
        let relay_circuit = Circuit::derive_relay_side(&relay_secret, &client_pk, cid);
        let decoded = decode_relay_request(
            &relay_circuit.circuit_key,
            &circuit.circuit_id,
            seq_no,
            &encoded,
        )
        .unwrap();

        assert_eq!(decoded, payload);
    }

    #[test]
    fn response_roundtrip() {
        let relay_secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let relay_pk = X25519PublicKey::from(&relay_secret);
        let circuit = Circuit::new(&relay_pk);

        let seq_no = 42u64;
        let response_payload = b"InboxMessages(...)";
        let encoded = encoder::encode_relay_response(
            &circuit.circuit_key,
            &circuit.circuit_id,
            seq_no,
            response_payload,
        )
        .unwrap();

        let decoded =
            decode_relay_response(&circuit.circuit_key, &circuit.circuit_id, &encoded).unwrap();

        assert_eq!(decoded, response_payload);
    }

    #[test]
    fn full_relay_flow() {
        let relay_secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let relay_pk = X25519PublicKey::from(&relay_secret);
        let circuit = Circuit::new(&relay_pk);

        // Client → Relay: request
        let req_payload = b"fetch inbox 123";
        let onion_req = encoder::encode_relay_request(&circuit, req_payload).unwrap();

        // Relay: decode request
        let (client_pk, cid, seq_no) = extract_request_header(&onion_req).unwrap();
        let relay_circuit = Circuit::derive_relay_side(&relay_secret, &client_pk, cid);
        let decoded_req = decode_relay_request(
            &relay_circuit.circuit_key,
            &circuit.circuit_id,
            seq_no,
            &onion_req,
        )
        .unwrap();
        assert_eq!(decoded_req, req_payload);

        // Relay → Client: response (uses same seq_no)
        let resp_payload = b"here are your messages";
        let onion_resp = encoder::encode_relay_response(
            &relay_circuit.circuit_key,
            &circuit.circuit_id,
            seq_no,
            resp_payload,
        )
        .unwrap();

        // Client: decode response
        let decoded_resp =
            decode_relay_response(&circuit.circuit_key, &circuit.circuit_id, &onion_resp).unwrap();
        assert_eq!(decoded_resp, resp_payload);
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let relay_secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let relay_pk = X25519PublicKey::from(&relay_secret);
        let circuit = Circuit::new(&relay_pk);

        let mut encoded = encoder::encode_relay_request(&circuit, b"test").unwrap();
        // Flip a byte in the ciphertext
        let last = encoded.len() - 1;
        encoded[last] ^= 0xFF;

        let (client_pk, cid, seq_no) = extract_request_header(&encoded).unwrap();
        let relay_circuit = Circuit::derive_relay_side(&relay_secret, &client_pk, cid);
        let result = decode_relay_request(
            &relay_circuit.circuit_key,
            &circuit.circuit_id,
            seq_no,
            &encoded,
        );
        assert!(result.is_err());
    }

    #[test]
    fn different_messages_different_nonces() {
        let relay_secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let relay_pk = X25519PublicKey::from(&relay_secret);
        let circuit = Circuit::new(&relay_pk);

        let enc1 = encoder::encode_relay_request(&circuit, b"msg1").unwrap();
        let enc2 = encoder::encode_relay_request(&circuit, b"msg1").unwrap();

        // Same plaintext but ciphertexts must differ (different nonces)
        assert_ne!(enc1, enc2);

        // Both must decode correctly
        let (pk1, cid1, seq1) = extract_request_header(&enc1).unwrap();
        let (_pk2, cid2, seq2) = extract_request_header(&enc2).unwrap();
        assert_eq!(seq1, 0);
        assert_eq!(seq2, 1);

        let rc = Circuit::derive_relay_side(&relay_secret, &pk1, cid1);
        let d1 = decode_relay_request(&rc.circuit_key, &cid1, seq1, &enc1).unwrap();
        let d2 = decode_relay_request(&rc.circuit_key, &cid2, seq2, &enc2).unwrap();
        assert_eq!(d1, b"msg1");
        assert_eq!(d2, b"msg1");
    }
}
