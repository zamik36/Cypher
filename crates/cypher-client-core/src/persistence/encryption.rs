//! Shared AES-256-GCM encryption/decryption and zstd compression helpers.
//!
//! Used by both [`SqliteMessageStore`](super::sqlite::SqliteMessageStore) and
//! [`IdentityStore`](crate::identity_store::IdentityStore).

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use cypher_common::{Error, Result};
use rand::rngs::OsRng;
use rand::RngCore;

/// AES-256-GCM nonce length in bytes.
pub const NONCE_LEN: usize = 12;

/// Messages shorter than this are stored without zstd compression.
pub const COMPRESSION_THRESHOLD: usize = 64;

/// Default zstd compression level (good speed/ratio balance).
pub const ZSTD_LEVEL: i32 = 3;

/// Encrypt `data` with AES-256-GCM using the given key and a random nonce.
pub fn encrypt(key: &[u8; 32], data: &[u8]) -> Result<(Vec<u8>, [u8; NONCE_LEN])> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| Error::Crypto(format!("AES init: {e}")))?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, data)
        .map_err(|e| Error::Crypto(format!("encrypt: {e}")))?;
    Ok((ct, nonce_bytes))
}

/// Decrypt `ciphertext` with AES-256-GCM using the given key and nonce.
pub fn decrypt(key: &[u8; 32], ciphertext: &[u8], nonce_bytes: &[u8]) -> Result<Vec<u8>> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| Error::Crypto(format!("AES init: {e}")))?;
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| Error::Crypto(format!("decrypt: {e}")))
}

/// Optionally compress data with zstd if it exceeds the threshold.
/// Returns `(data, was_compressed)`.
pub fn maybe_compress(data: &[u8]) -> Result<(Vec<u8>, bool)> {
    if data.len() > COMPRESSION_THRESHOLD {
        let compressed = zstd::encode_all(data, ZSTD_LEVEL)
            .map_err(|e| Error::Crypto(format!("zstd compress: {e}")))?;
        Ok((compressed, true))
    } else {
        Ok((data.to_vec(), false))
    }
}

/// Decompress data with zstd if the `compressed` flag is set.
pub fn maybe_decompress(data: Vec<u8>, compressed: bool) -> Result<Vec<u8>> {
    if compressed {
        zstd::decode_all(data.as_slice())
            .map_err(|e| Error::Crypto(format!("zstd decompress: {e}")))
    } else {
        Ok(data)
    }
}

/// Compress, then encrypt data. Returns `(ciphertext, nonce, was_compressed)`.
pub fn compress_and_encrypt(
    key: &[u8; 32],
    data: &[u8],
) -> Result<(Vec<u8>, [u8; NONCE_LEN], bool)> {
    let (prepared, compressed) = maybe_compress(data)?;
    let (ct, nonce) = encrypt(key, &prepared)?;
    Ok((ct, nonce, compressed))
}

/// Decrypt, then decompress data.
pub fn decrypt_and_decompress(
    key: &[u8; 32],
    ciphertext: &[u8],
    nonce: &[u8],
    compressed: bool,
) -> Result<Vec<u8>> {
    let decrypted = decrypt(key, ciphertext, nonce)?;
    maybe_decompress(decrypted, compressed)
}
