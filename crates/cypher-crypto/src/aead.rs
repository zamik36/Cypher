use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};

use crate::error::CryptoError;

pub const TAG_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;

/// AES-256-GCM seal with an explicit nonce. The caller guarantees the
/// `(key, nonce)` pair is never reused.
pub fn seal(key: &[u8; 32], nonce: &[u8; NONCE_LEN], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(plaintext.len().saturating_add(TAG_LEN));
    buf.extend_from_slice(plaintext);
    seal_in_place(key, nonce, aad, &mut buf);
    buf
}

#[expect(
    clippy::expect_used,
    reason = "a Vec always grows to fit the tag; only plaintexts over 64 GiB fail"
)]
pub fn seal_in_place(key: &[u8; 32], nonce: &[u8; NONCE_LEN], aad: &[u8], buf: &mut Vec<u8>) {
    Aes256Gcm::new(key.into())
        .encrypt_in_place(Nonce::from_slice(nonce), aad, buf)
        .expect("Vec buffer grows to fit the tag");
}

/// Encrypts `buf` in place and returns the detached tag.
#[expect(
    clippy::expect_used,
    reason = "only plaintexts over the 64 GiB AES-GCM limit fail; chunks are at most 1 MiB"
)]
pub fn seal_detached(
    key: &[u8; 32],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    buf: &mut [u8],
) -> [u8; TAG_LEN] {
    Aes256Gcm::new(key.into())
        .encrypt_in_place_detached(Nonce::from_slice(nonce), aad, buf)
        .expect("plaintext length is within AES-GCM limits")
        .into()
}

pub fn open(
    key: &[u8; 32],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let mut buf = ciphertext.to_vec();
    open_in_place(key, nonce, aad, &mut buf)?;
    Ok(buf)
}

pub fn open_in_place(
    key: &[u8; 32],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    buf: &mut Vec<u8>,
) -> Result<(), CryptoError> {
    Aes256Gcm::new(key.into())
        .decrypt_in_place(Nonce::from_slice(nonce), aad, buf)
        .map_err(|_| CryptoError::Aead)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [42u8; 32];
    const NONCE: [u8; 12] = [9u8; 12];

    #[test]
    fn seal_open_roundtrip() {
        let ct = seal(&KEY, &NONCE, b"aad", b"hello");
        assert_eq!(ct.len(), 5 + TAG_LEN);
        assert_eq!(open(&KEY, &NONCE, b"aad", &ct).unwrap(), b"hello");
    }

    #[test]
    fn wrong_key_nonce_or_aad_fails() {
        let ct = seal(&KEY, &NONCE, b"aad", b"secret");
        assert_eq!(
            open(&[43u8; 32], &NONCE, b"aad", &ct),
            Err(CryptoError::Aead)
        );
        assert_eq!(open(&KEY, &[0u8; 12], b"aad", &ct), Err(CryptoError::Aead));
        assert_eq!(open(&KEY, &NONCE, b"other", &ct), Err(CryptoError::Aead));
    }

    #[test]
    fn truncated_ciphertext_fails() {
        assert_eq!(open(&KEY, &NONCE, b"", &[0u8; 3]), Err(CryptoError::Aead));
    }
}
