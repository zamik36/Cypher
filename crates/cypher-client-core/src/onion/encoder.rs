use cypher_common::Result;
use cypher_crypto::aead_encrypt;

use super::circuit::Circuit;

/// Onion control message prefix byte.
pub const ONION_PREFIX: u8 = 0x02;

/// Onion subtypes.
pub const SUBTYPE_RELAY_REQUEST: u8 = 0x02;
pub const SUBTYPE_RELAY_RESPONSE: u8 = 0x03;

/// Encode a relay request into an onion-wrapped message.
///
/// **Wire format produced:**
/// ```text
/// [0x02 onion prefix][32B ephemeral_public][16B circuit_id][8B seq_no LE][AEAD ciphertext]
/// ```
///
/// Each message uses a unique nonce derived from `circuit_id || seq_no` to
/// prevent AES-GCM nonce reuse. The seq_no is sent in cleartext so the relay
/// can reconstruct the same nonce for decryption.
///
/// **Inner plaintext (before AEAD):**
/// ```text
/// [0x02 subtype][16B circuit_id][u32 LE payload_len][payload]
/// ```
pub fn encode_relay_request(circuit: &Circuit, request_payload: &[u8]) -> Result<Vec<u8>> {
    // Get unique nonce material (circuit_id || seq_no)
    let nonce_material = circuit.next_nonce_material();
    let seq_no = &nonce_material[16..24]; // last 8 bytes = seq_no LE

    // Build inner plaintext
    let mut inner = Vec::with_capacity(1 + 16 + 4 + request_payload.len());
    inner.push(SUBTYPE_RELAY_REQUEST);
    inner.extend_from_slice(&circuit.circuit_id);
    inner.extend_from_slice(&(request_payload.len() as u32).to_le_bytes());
    inner.extend_from_slice(request_payload);

    // AEAD encrypt: key=circuit_key, nonce=HKDF(circuit_id||seq_no), AAD=circuit_id
    let ciphertext = aead_encrypt(
        &circuit.circuit_key,
        &nonce_material,
        &inner,
        &circuit.circuit_id,
    )?;

    // Assemble outer message: prefix + pk + circuit_id + seq_no + ciphertext
    let mut out = Vec::with_capacity(1 + 32 + 16 + 8 + ciphertext.len());
    out.push(ONION_PREFIX);
    out.extend_from_slice(circuit.ephemeral_public.as_bytes());
    out.extend_from_slice(&circuit.circuit_id);
    out.extend_from_slice(seq_no);
    out.extend_from_slice(&ciphertext);

    Ok(out)
}

/// Encode a relay response (used by the relay to wrap signaling responses back
/// to the client).
///
/// **Wire format:** `[0x02 prefix][8B seq_no LE][AEAD ciphertext of inner]`
///
/// **Inner plaintext:** `[0x03 subtype][16B circuit_id][u32 LE payload_len][payload]`
///
/// The relay uses the same `seq_no` from the request so both sides derive the
/// same nonce for the response direction. To separate request/response nonce
/// domains, the response nonce material is `circuit_id || (seq_no | 0x80...00)`.
pub fn encode_relay_response(
    circuit_key: &[u8; 32],
    circuit_id: &[u8; 16],
    seq_no: u64,
    response_payload: &[u8],
) -> Result<Vec<u8>> {
    let nonce_material = response_nonce_material(circuit_id, seq_no);

    let mut inner = Vec::with_capacity(1 + 16 + 4 + response_payload.len());
    inner.push(SUBTYPE_RELAY_RESPONSE);
    inner.extend_from_slice(circuit_id);
    inner.extend_from_slice(&(response_payload.len() as u32).to_le_bytes());
    inner.extend_from_slice(response_payload);

    let ciphertext = aead_encrypt(circuit_key, &nonce_material, &inner, circuit_id)?;

    let mut out = Vec::with_capacity(1 + 8 + ciphertext.len());
    out.push(ONION_PREFIX);
    out.extend_from_slice(&seq_no.to_le_bytes());
    out.extend_from_slice(&ciphertext);

    Ok(out)
}

/// Build nonce material for response direction: circuit_id || (seq_no | MSB set).
///
/// Setting the MSB of seq_no ensures request and response nonces for the same
/// seq_no never collide, giving each direction its own nonce space.
pub fn response_nonce_material(circuit_id: &[u8; 16], seq_no: u64) -> [u8; 24] {
    let response_seq = seq_no | (1u64 << 63);
    let mut material = [0u8; 24];
    material[..16].copy_from_slice(circuit_id);
    material[16..].copy_from_slice(&response_seq.to_le_bytes());
    material
}
