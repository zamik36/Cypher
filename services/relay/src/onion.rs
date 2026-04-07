//! Onion relay handler — decrypts 1-hop onion requests, forwards to signaling,
//! and re-encrypts responses back to the client.

use std::collections::HashMap;
use std::time::Duration;

use bytes::Bytes;
use cypher_transport::frame::{Frame, FrameFlags};
use futures::StreamExt;
use hkdf::Hkdf;
use sha2::Sha256;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;
use tracing::{debug, info, warn};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};
use zeroize::Zeroize;

use cypher_proto::{dispatch, Message};
use cypher_transport::codec::FrameCodec;

use crate::BoxedStream;

/// Onion prefix byte (matches cypher-client-core::onion::encoder::ONION_PREFIX).
const ONION_PREFIX: u8 = 0x02;
const SUBTYPE_RELAY_REQUEST: u8 = 0x02;
const SUBTYPE_RELAY_RESPONSE: u8 = 0x03;

/// Maximum time to wait for a NATS response from signaling.
const NATS_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum number of circuits per onion connection.
const MAX_CIRCUITS: usize = 16;

/// Circuit cache entry: (circuit_key, max_seen_seq_no).
/// Key is (ephemeral_pk_bytes, circuit_id).
type CircuitCache = HashMap<([u8; 32], [u8; 16]), ([u8; 32], u64)>;

/// Run the onion relay loop for a single client connection.
///
/// The client's first frame was `b"ONION"`, identifying this as an onion session.
/// All subsequent frames are onion-encrypted relay requests.
pub async fn handle_onion_connection(
    relay_secret: &StaticSecret,
    nats: &async_nats::Client,
    writer: &mpsc::Sender<Frame>,
    reader: &mut futures::stream::SplitStream<Framed<BoxedStream, FrameCodec>>,
    seq_counter: &mut u32,
) {
    // Cache: (ephemeral_pk_bytes, circuit_id) -> (circuit_key, max_seen_seq).
    // Avoids re-deriving DH for every request on the same circuit.
    // max_seen_seq tracks the highest seq_no to reject replays.
    let mut circuits: CircuitCache = HashMap::new();

    while let Some(result) = reader.next().await {
        let frame = match result {
            Ok(f) => f,
            Err(e) => {
                debug!("onion relay reader error: {}", e);
                break;
            }
        };

        if frame.flags.contains(FrameFlags::PING) {
            let pong = Frame::new(0, frame.seq_no, FrameFlags::PONG, Bytes::new());
            if writer.send(pong).await.is_err() {
                break;
            }
            continue;
        }

        if frame.flags.contains(FrameFlags::SESSION_CLOSE) {
            debug!("onion relay: client sent SESSION_CLOSE");
            break;
        }

        let payload = &frame.payload;
        if payload.is_empty() || payload[0] != ONION_PREFIX {
            debug!("onion relay: non-onion frame, ignoring");
            continue;
        }

        match process_onion_request(relay_secret, nats, payload, &mut circuits).await {
            Ok(response) => {
                *seq_counter += 1;
                let resp_frame = Frame::new(*seq_counter, frame.seq_no, FrameFlags::NONE, response);
                if writer.send(resp_frame).await.is_err() {
                    debug!("onion relay: writer closed");
                    break;
                }
            }
            Err(e) => {
                warn!("onion relay: error processing request: {}", e);
            }
        }
    }

    // Zeroize all circuit keys on connection close.
    for (key, _) in circuits.values_mut() {
        key.zeroize();
    }
    info!("onion relay connection closed");
}

/// Derive a circuit key from the relay's static secret and the client's
/// ephemeral public key, using HKDF-SHA256 (same derivation as client-side).
fn derive_circuit_key(relay_secret: &StaticSecret, client_ek: &X25519PublicKey) -> [u8; 32] {
    let mut dh_shared = relay_secret.diffie_hellman(client_ek).to_bytes();
    let hk = Hkdf::<Sha256>::new(Some(b"cypher-circuit-v1"), &dh_shared);
    let mut key = [0u8; 32];
    hk.expand(b"circuit-key", &mut key)
        .expect("32 bytes is valid HKDF-SHA256 output");
    dh_shared.zeroize();
    key
}

/// Build nonce material for response direction: circuit_id || (seq_no | MSB set).
fn response_nonce_material(circuit_id: &[u8; 16], seq_no: u64) -> [u8; 24] {
    let response_seq = seq_no | (1u64 << 63);
    let mut material = [0u8; 24];
    material[..16].copy_from_slice(circuit_id);
    material[16..].copy_from_slice(&response_seq.to_le_bytes());
    material
}

/// Process a single onion-encrypted request:
/// 1. Extract ephemeral public key + circuit_id + seq_no from cleartext header
/// 2. Derive circuit key (cache-first to avoid repeated DH)
/// 3. Decrypt request using per-message nonce (circuit_id || seq_no)
/// 4. Forward to signaling via NATS request-reply
/// 5. Encrypt and return response using response nonce domain
async fn process_onion_request(
    relay_secret: &StaticSecret,
    nats: &async_nats::Client,
    data: &[u8],
    circuits: &mut CircuitCache,
) -> anyhow::Result<Bytes> {
    // Wire format: [0x02][32B ephemeral_pk][16B circuit_id][8B seq_no LE][AEAD ciphertext]
    if data.len() < 1 + 32 + 16 + 8 + 1 {
        anyhow::bail!("onion request too short");
    }

    let mut pk_bytes = [0u8; 32];
    pk_bytes.copy_from_slice(&data[1..33]);
    let client_ek = X25519PublicKey::from(pk_bytes);

    let mut circuit_id = [0u8; 16];
    circuit_id.copy_from_slice(&data[33..49]);

    let seq_no = u64::from_le_bytes(data[49..57].try_into().unwrap());
    let ciphertext = &data[57..];

    // Cache-first: only derive DH if we haven't seen this (pk, circuit_id) before.
    // If the cache is full and this is an unknown circuit, reject to prevent
    // a DoS where the client forces unbounded DH derivations.
    let cache_key = (pk_bytes, circuit_id);
    let circuit_key = if let Some(entry) = circuits.get_mut(&cache_key) {
        // Replay protection: reject seq_no <= last seen for this circuit.
        if seq_no <= entry.1 {
            anyhow::bail!(
                "replayed or out-of-order seq_no {seq_no} (last: {})",
                entry.1
            );
        }
        entry.1 = seq_no;
        entry.0
    } else {
        if circuits.len() >= MAX_CIRCUITS {
            anyhow::bail!("circuit limit reached ({MAX_CIRCUITS}), rejecting new circuit");
        }
        let key = derive_circuit_key(relay_secret, &client_ek);
        circuits.insert(cache_key, (key, seq_no));
        key
    };

    // Build per-message nonce material: circuit_id || seq_no
    let mut nonce_material = [0u8; 24];
    nonce_material[..16].copy_from_slice(&circuit_id);
    nonce_material[16..].copy_from_slice(&seq_no.to_le_bytes());

    // Decrypt.
    let inner = cypher_crypto::aead_decrypt(&circuit_key, &nonce_material, ciphertext, &circuit_id)
        .map_err(|e| anyhow::anyhow!("AEAD decrypt failed: {e}"))?;

    // Parse inner: [subtype 1B][circuit_id 16B][payload_len 4B LE][payload]
    if inner.len() < 1 + 16 + 4 {
        anyhow::bail!("onion inner too short");
    }
    if inner[0] != SUBTYPE_RELAY_REQUEST {
        anyhow::bail!("unexpected subtype: 0x{:02x}", inner[0]);
    }

    let payload_len = u32::from_le_bytes(inner[17..21].try_into().unwrap()) as usize;
    if 21 + payload_len > inner.len() {
        anyhow::bail!("inner payload length exceeds data");
    }
    let request_payload = &inner[21..21 + payload_len];

    // Forward to signaling via NATS request-reply.
    let envelope = serde_json::json!({
        "session_id": 0u64,
        "payload": request_payload,
    });
    let nats_payload = Bytes::from(serde_json::to_vec(&envelope)?);

    let subject = match dispatch(request_payload)? {
        Message::InboxFetch(_) => "signaling.inbox_fetch",
        Message::InboxAck(_) => "signaling.inbox_ack",
        other => anyhow::bail!("unsupported onion request payload: {other:?}"),
    };

    let nats_response = tokio::time::timeout(
        NATS_TIMEOUT,
        nats.request(subject.to_string(), nats_payload),
    )
    .await
    .map_err(|_| anyhow::anyhow!("NATS request to signaling timed out"))?
    .map_err(|e| anyhow::anyhow!("NATS request failed: {e}"))?;

    let response_payload = nats_response.payload.to_vec();

    // Encrypt response back to client using response nonce domain.
    let resp_nonce = response_nonce_material(&circuit_id, seq_no);

    let mut resp_inner = Vec::with_capacity(1 + 16 + 4 + response_payload.len());
    resp_inner.push(SUBTYPE_RELAY_RESPONSE);
    resp_inner.extend_from_slice(&circuit_id);
    resp_inner.extend_from_slice(&(response_payload.len() as u32).to_le_bytes());
    resp_inner.extend_from_slice(&response_payload);

    let resp_ciphertext =
        cypher_crypto::aead_encrypt(&circuit_key, &resp_nonce, &resp_inner, &circuit_id)
            .map_err(|e| anyhow::anyhow!("AEAD encrypt response failed: {e}"))?;

    // Response wire format: [0x02][8B seq_no LE][ciphertext]
    let mut out = Vec::with_capacity(1 + 8 + resp_ciphertext.len());
    out.push(ONION_PREFIX);
    out.extend_from_slice(&seq_no.to_le_bytes());
    out.extend_from_slice(&resp_ciphertext);

    Ok(Bytes::from(out))
}
