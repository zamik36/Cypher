//! Sealed-sender envelope (ECIES over X25519 + AES-256-GCM) for offline
//! delivery: the storage server learns neither the sender nor the content.

use rand_core::CryptoRngCore;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::aead::{self, TAG_LEN};
use crate::double_ratchet::dh;
use crate::error::CryptoError;
use crate::kdf::hkdf;

pub const SEALED_OVERHEAD: usize = 32 + TAG_LEN;

const NONCE: [u8; 12] = [0; 12];

pub fn seal(
    recipient_dh: &[u8; 32],
    plaintext: &[u8],
    rng: &mut impl CryptoRngCore,
) -> Result<Vec<u8>, CryptoError> {
    let eph = StaticSecret::random_from_rng(rng);
    let eph_pub = PublicKey::from(&eph).to_bytes();
    let binding = binding(&eph_pub, recipient_dh);
    let key = hkdf::<32>(
        Some(&binding),
        &*dh(&eph, recipient_dh)?,
        b"cypher/v2/sealed",
    );

    let mut out = Vec::with_capacity(plaintext.len().saturating_add(SEALED_OVERHEAD));
    out.extend_from_slice(&eph_pub);
    out.extend_from_slice(plaintext);
    let (_, body) = out.split_at_mut(eph_pub.len());
    let tag = aead::seal_detached(&key, &NONCE, &binding, body);
    out.extend_from_slice(&tag);
    Ok(out)
}

pub fn open(recipient: &StaticSecret, sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if sealed.len() < SEALED_OVERHEAD {
        return Err(CryptoError::Malformed);
    }
    let (eph_pub, body) = sealed.split_at(32);
    let eph_pub: [u8; 32] = eph_pub.try_into().map_err(|_| CryptoError::Malformed)?;
    let recipient_pub = PublicKey::from(recipient).to_bytes();
    let binding = binding(&eph_pub, &recipient_pub);
    let key = hkdf::<32>(
        Some(&binding),
        &*dh(recipient, &eph_pub)?,
        b"cypher/v2/sealed",
    );
    aead::open(&key, &NONCE, &binding, body)
}

fn binding(eph: &[u8; 32], recipient: &[u8; 32]) -> [u8; 64] {
    let mut b = [0u8; 64];
    b[..32].copy_from_slice(eph);
    b[32..].copy_from_slice(recipient);
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn seal_open_roundtrip() {
        let sk = StaticSecret::random_from_rng(OsRng);
        let pk = PublicKey::from(&sk).to_bytes();
        let sealed = seal(&pk, b"offline hello", &mut OsRng).unwrap();
        assert_eq!(sealed.len(), SEALED_OVERHEAD + 13);
        assert_eq!(open(&sk, &sealed).unwrap(), b"offline hello");
    }

    #[test]
    fn wrong_recipient_or_tamper_fails() {
        let sk = StaticSecret::random_from_rng(OsRng);
        let pk = PublicKey::from(&sk).to_bytes();
        let mut sealed = seal(&pk, b"x", &mut OsRng).unwrap();
        let other = StaticSecret::random_from_rng(OsRng);
        assert_eq!(open(&other, &sealed), Err(CryptoError::Aead));
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert_eq!(open(&sk, &sealed), Err(CryptoError::Aead));
        assert_eq!(open(&sk, &sealed[..10]), Err(CryptoError::Malformed));
    }

    #[test]
    fn low_order_ephemeral_is_rejected() {
        let sk = StaticSecret::random_from_rng(OsRng);
        let mut forged = vec![0u8; SEALED_OVERHEAD + 4];
        forged[0] = 0;
        assert_eq!(open(&sk, &forged), Err(CryptoError::NonContributory));
    }
}
