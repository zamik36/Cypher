//! One-hop onion for anonymous requests: the relay learns the client's IP
//! but not the request; the signaling service learns the request but not
//! the IP. Requests are sealed to the signaling onion key and carry a fresh
//! reply key; both directions are padded to fixed buckets. Every response
//! has its own random nonce, so a replayed request that is answered twice
//! never reuses a nonce under its reply key.

use rand_core::CryptoRngCore;
use x25519_dalek::StaticSecret;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::aead;
use crate::error::CryptoError;
use crate::reader::Reader;
use crate::sealed;

const LARGEST_BUCKET: usize = 96 * 1024;
const BUCKETS: [usize; 4] = [1024, 4096, 16 * 1024, LARGEST_BUCKET];
const RESPONSE_AAD: &[u8] = b"cypher/v2/onion-response";

/// Largest frame that fits in a sealed onion request.
pub const MAX_ONION_FRAME: usize = LARGEST_BUCKET - 32 - 8 - 4;

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct ReplyKey([u8; 32]);

pub struct OpenedRequest {
    pub frame: Vec<u8>,
    pub timestamp_secs: u64,
    pub reply: ReplyKey,
    /// The sealing's ephemeral key: fresh for every request and covered by
    /// its authentication, so a replay carries the same id.
    pub replay_id: [u8; 32],
}

pub fn seal_request(
    onion_key: &[u8; 32],
    frame: &[u8],
    now_secs: u64,
    rng: &mut impl CryptoRngCore,
) -> Result<(Vec<u8>, ReplyKey), CryptoError> {
    if frame.len() > MAX_ONION_FRAME {
        return Err(CryptoError::Malformed);
    }
    let mut reply = [0u8; 32];
    rng.fill_bytes(&mut reply);
    let reply = ReplyKey(reply);

    let mut plain = Vec::with_capacity(bucket(frame.len().saturating_add(44)));
    plain.extend_from_slice(&reply.0);
    plain.extend_from_slice(&now_secs.to_le_bytes());
    plain.extend_from_slice(
        &u32::try_from(frame.len())
            .map_err(|_| CryptoError::Malformed)?
            .to_le_bytes(),
    );
    plain.extend_from_slice(frame);
    plain.resize(bucket(plain.len()), 0);
    let sealed = sealed::seal(onion_key, &plain, rng);
    plain.zeroize();
    Ok((sealed?, reply))
}

pub fn open_request(
    onion_secret: &StaticSecret,
    blob: &[u8],
) -> Result<OpenedRequest, CryptoError> {
    let mut plain = sealed::open(onion_secret, blob)?;
    let result = parse_request(&plain);
    plain.zeroize();
    let replay_id = *blob.first_chunk::<32>().ok_or(CryptoError::Malformed)?;
    result.map(|opened| OpenedRequest {
        replay_id,
        ..opened
    })
}

fn parse_request(plain: &[u8]) -> Result<OpenedRequest, CryptoError> {
    let mut r = Reader::new(plain);
    let reply = ReplyKey(r.array()?);
    let timestamp_secs = u64::from_le_bytes(r.array()?);
    let len = r.u32()? as usize;
    let frame = r.take(len)?.to_vec();
    Ok(OpenedRequest {
        frame,
        timestamp_secs,
        reply,
        replay_id: [0; 32],
    })
}

/// `nonce ‖ AES-256-GCM(len ‖ frame ‖ padding)` under the request's reply key.
pub fn seal_response(reply: &ReplyKey, frame: &[u8], rng: &mut impl CryptoRngCore) -> Vec<u8> {
    let mut nonce = [0u8; aead::NONCE_LEN];
    rng.fill_bytes(&mut nonce);
    let mut plain =
        Vec::with_capacity(bucket(frame.len().saturating_add(4)).saturating_add(aead::TAG_LEN));
    plain.extend_from_slice(&u32::try_from(frame.len()).unwrap_or(u32::MAX).to_le_bytes());
    plain.extend_from_slice(frame);
    plain.resize(bucket(plain.len()), 0);
    aead::seal_in_place(&reply.0, &nonce, RESPONSE_AAD, &mut plain);
    let mut out = Vec::with_capacity(aead::NONCE_LEN.saturating_add(plain.len()));
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&plain);
    out
}

pub fn open_response(reply: &ReplyKey, blob: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let (nonce, sealed) = blob
        .split_first_chunk::<{ aead::NONCE_LEN }>()
        .ok_or(CryptoError::Malformed)?;
    let plain = aead::open(&reply.0, nonce, RESPONSE_AAD, sealed)?;
    let mut r = Reader::new(&plain);
    let len = r.u32()? as usize;
    Ok(r.take(len)?.to_vec())
}

fn bucket(n: usize) -> usize {
    BUCKETS
        .iter()
        .copied()
        .find(|&b| b >= n)
        .unwrap_or_else(|| n.next_multiple_of(LARGEST_BUCKET))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;
    use x25519_dalek::PublicKey;

    fn keys() -> (StaticSecret, [u8; 32]) {
        let sk = StaticSecret::random_from_rng(OsRng);
        let pk = PublicKey::from(&sk).to_bytes();
        (sk, pk)
    }

    #[test]
    fn request_response_roundtrip_with_uniform_sizes() {
        let (sk, pk) = keys();
        let (blob_small, reply) = seal_request(&pk, b"fetch", 1_700_000_000, &mut OsRng).unwrap();
        let (blob_other, _) = seal_request(&pk, &[7u8; 900], 1, &mut OsRng).unwrap();
        assert_eq!(blob_small.len(), blob_other.len(), "requests are bucketed");

        let opened = open_request(&sk, &blob_small).unwrap();
        assert_eq!(opened.frame, b"fetch");
        assert_eq!(opened.timestamp_secs, 1_700_000_000);
        let again = open_request(&sk, &blob_small).unwrap();
        let other = open_request(&sk, &blob_other).unwrap();
        assert_eq!(opened.replay_id, again.replay_id, "a replay keeps its id");
        assert_ne!(opened.replay_id, other.replay_id);

        let resp = seal_response(&opened.reply, b"batch", &mut OsRng);
        assert_eq!(open_response(&reply, &resp).unwrap(), b"batch");
    }

    /// A relay may replay a request so that it is answered twice: the two
    /// responses must not share a nonce, or AES-GCM would leak both.
    #[test]
    fn responses_under_one_reply_key_never_share_a_nonce() {
        let (sk, pk) = keys();
        let (blob, reply) = seal_request(&pk, b"fetch", 0, &mut OsRng).unwrap();
        let opened = open_request(&sk, &blob).unwrap();
        let first = seal_response(&opened.reply, b"batch", &mut OsRng);
        let second = seal_response(&opened.reply, b"", &mut OsRng);
        assert_eq!(first.len(), second.len(), "responses are bucketed");
        assert_ne!(first[..aead::NONCE_LEN], second[..aead::NONCE_LEN]);
        assert_eq!(open_response(&reply, &first).unwrap(), b"batch");
        assert_eq!(open_response(&reply, &second).unwrap(), b"");
        open_response(&reply, &first[..aead::NONCE_LEN - 1]).unwrap_err();
    }

    #[test]
    fn wrong_keys_and_tampering_fail() {
        let (sk, pk) = keys();
        let (other_sk, _) = keys();
        let (mut blob, reply) = seal_request(&pk, b"x", 0, &mut OsRng).unwrap();
        assert!(open_request(&other_sk, &blob).is_err());
        blob[40] ^= 1;
        assert!(open_request(&sk, &blob).is_err());

        let resp = seal_response(&reply, b"y", &mut OsRng);
        let (_, other_reply) = seal_request(&pk, b"z", 0, &mut OsRng).unwrap();
        open_response(&other_reply, &resp).unwrap_err();
    }

    #[test]
    #[cfg_attr(miri, ignore = "seals a maximum-size frame; minutes under Miri")]
    fn oversized_frames_are_rejected() {
        let (_, pk) = keys();
        assert!(seal_request(&pk, &vec![0u8; MAX_ONION_FRAME + 1], 0, &mut OsRng).is_err());
        seal_request(&pk, &vec![0u8; MAX_ONION_FRAME], 0, &mut OsRng).unwrap();
    }
}
