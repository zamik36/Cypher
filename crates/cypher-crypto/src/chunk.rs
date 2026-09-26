//! Per-file chunk encryption. Every file gets a fresh random key that travels
//! only inside the end-to-end encrypted file descriptor, so chunks never touch
//! the chat ratchet and can be sealed in parallel or resumed after restart.

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use cypher_types::FileId;
use rand_core::CryptoRngCore;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::aead::TAG_LEN;
use crate::error::CryptoError;

pub const CHUNK_TAG_LEN: usize = TAG_LEN;

#[derive(Clone, Zeroize, ZeroizeOnDrop, PartialEq, Eq)]
pub struct FileKey([u8; 32]);

impl FileKey {
    pub fn random(rng: &mut impl CryptoRngCore) -> Self {
        let mut k = [0u8; 32];
        rng.fill_bytes(&mut k);
        Self(k)
    }

    pub fn from_bytes(k: [u8; 32]) -> Self {
        Self(k)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for FileKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FileKey(..)")
    }
}

/// AES-256-GCM keyed once per file. The nonce is the chunk index, which is
/// unique under a single-use key; the AAD binds file id, chunk count, index
/// and a last-chunk flag, preventing reordering, splicing and truncation.
#[derive(Clone)]
pub struct ChunkCipher {
    aes: Aes256Gcm,
    file_id: FileId,
    chunk_count: u32,
}

impl ChunkCipher {
    pub fn new(key: &FileKey, file_id: FileId, chunk_count: u32) -> Self {
        Self {
            aes: Aes256Gcm::new(key.as_bytes().into()),
            file_id,
            chunk_count,
        }
    }

    pub fn chunk_count(&self) -> u32 {
        self.chunk_count
    }

    /// Encrypts `buf` in place, appending the 16-byte tag.
    pub fn seal(&self, index: u32, buf: &mut Vec<u8>) -> Result<(), CryptoError> {
        let (nonce, aad) = self.params(index)?;
        self.aes
            .encrypt_in_place(Nonce::from_slice(&nonce), &aad, buf)
            .map_err(|_| CryptoError::Malformed)
    }

    /// Decrypts `buf` in place, removing the tag.
    pub fn open(&self, index: u32, buf: &mut Vec<u8>) -> Result<(), CryptoError> {
        let (nonce, aad) = self.params(index)?;
        self.aes
            .decrypt_in_place(Nonce::from_slice(&nonce), &aad, buf)
            .map_err(|_| CryptoError::Aead)
    }

    fn params(&self, index: u32) -> Result<([u8; 12], [u8; 25]), CryptoError> {
        if index >= self.chunk_count {
            return Err(CryptoError::Malformed);
        }
        let mut nonce = [0u8; 12];
        nonce[8..].copy_from_slice(&index.to_be_bytes());
        let mut aad = [0u8; 25];
        aad[..16].copy_from_slice(self.file_id.as_bytes());
        aad[16..20].copy_from_slice(&self.chunk_count.to_le_bytes());
        aad[20..24].copy_from_slice(&index.to_le_bytes());
        aad[24] = u8::from(index + 1 == self.chunk_count);
        Ok((nonce, aad))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    fn cipher(count: u32) -> ChunkCipher {
        ChunkCipher::new(&FileKey::random(&mut OsRng), FileId([1; 16]), count)
    }

    #[test]
    fn seal_open_roundtrip() {
        let c = cipher(3);
        let mut buf = b"chunk-data".to_vec();
        c.seal(1, &mut buf).unwrap();
        assert_eq!(buf.len(), 10 + CHUNK_TAG_LEN);
        c.open(1, &mut buf).unwrap();
        assert_eq!(buf, b"chunk-data");
    }

    #[test]
    fn reordered_or_out_of_range_chunks_fail() {
        let c = cipher(3);
        let mut buf = b"x".to_vec();
        c.seal(0, &mut buf).unwrap();
        let mut moved = buf.clone();
        assert_eq!(c.open(1, &mut moved), Err(CryptoError::Aead));
        assert_eq!(c.seal(3, &mut b"y".to_vec()), Err(CryptoError::Malformed));
        assert_eq!(c.open(3, &mut buf), Err(CryptoError::Malformed));
    }

    #[test]
    fn truncation_is_detected() {
        let key = FileKey::random(&mut OsRng);
        let full = ChunkCipher::new(&key, FileId([2; 16]), 3);
        let truncated = ChunkCipher::new(&key, FileId([2; 16]), 2);
        let mut buf = b"middle".to_vec();
        full.seal(1, &mut buf).unwrap();
        assert_eq!(truncated.open(1, &mut buf), Err(CryptoError::Aead));
    }

    #[test]
    fn other_file_cannot_splice() {
        let key = FileKey::random(&mut OsRng);
        let a = ChunkCipher::new(&key, FileId([3; 16]), 2);
        let b = ChunkCipher::new(&key, FileId([4; 16]), 2);
        let mut buf = b"data".to_vec();
        a.seal(0, &mut buf).unwrap();
        assert_eq!(b.open(0, &mut buf), Err(CryptoError::Aead));
    }
}
