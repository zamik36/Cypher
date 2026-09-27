//! Playback of media stored sealed at rest: the file holds the transfer's
//! ciphertext chunks (`chunk_size + 16` bytes each) and [`MediaKey`], kept in
//! [`Table::Media`](crate::Table::Media), is what decrypts them on demand.

use std::ops::RangeInclusive;

use cypher_crypto::FileKey;
use cypher_crypto::chunk::CHUNK_TAG_LEN;
use cypher_types::FileId;
use serde::{Deserialize, Serialize};
use zeroize::ZeroizeOnDrop;

use crate::CoreError;
use crate::envelope::{FileDesc, chunk_count};

#[derive(Clone, Serialize, Deserialize, ZeroizeOnDrop)]
pub struct MediaKey {
    #[zeroize(skip)]
    pub file_id: FileId,
    #[zeroize(skip)]
    pub mime: String,
    #[zeroize(skip)]
    pub size: u64,
    #[zeroize(skip)]
    pub chunk_size: u32,
    key: [u8; 32],
}

impl std::fmt::Debug for MediaKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaKey")
            .field("file_id", &self.file_id)
            .field("mime", &self.mime)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl MediaKey {
    pub(crate) fn from_desc(desc: &FileDesc) -> Self {
        Self {
            file_id: desc.file_id,
            mime: desc.mime.clone(),
            size: desc.size,
            chunk_size: desc.chunk_size,
            key: desc.key,
        }
    }

    pub fn chunk_count(&self) -> u32 {
        chunk_count(self.size, self.chunk_size)
    }

    /// Size of the sealed file on disk.
    pub fn sealed_len(&self) -> u64 {
        self.size + u64::from(self.chunk_count()) * CHUNK_TAG_LEN as u64
    }

    /// Where chunk `index` lives in the sealed file and how long it is there.
    pub fn chunk_span(&self, index: u32) -> (u64, usize) {
        let start = u64::from(index) * u64::from(self.chunk_size);
        let plain = self
            .size
            .saturating_sub(start)
            .min(u64::from(self.chunk_size));
        let stride = u64::from(self.chunk_size) + CHUNK_TAG_LEN as u64;
        // Bounded by `chunk_size`, which is at most `MAX_CHUNK_SIZE`.
        (u64::from(index) * stride, plain as usize + CHUNK_TAG_LEN)
    }

    /// Chunks holding plaintext bytes `start..=end`; `None` when the range
    /// is empty or outside the file.
    pub fn chunks_covering(&self, start: u64, end: u64) -> Option<RangeInclusive<u32>> {
        if start > end || end >= self.size {
            return None;
        }
        let cs = u64::from(self.chunk_size);
        let first = u32::try_from(start / cs).ok()?;
        let last = u32::try_from(end / cs).ok()?;
        Some(first..=last)
    }

    /// Decrypts one sealed chunk in place; `buf` ends up holding plaintext.
    pub fn open_chunk(&self, index: u32, buf: &mut Vec<u8>) -> Result<(), CoreError> {
        if index >= self.chunk_count() || buf.len() != self.chunk_span(index).1 {
            return Err(CoreError::Crypto);
        }
        Ok(self.cipher().open(index, buf)?)
    }

    /// Decrypts a whole sealed file (for small media such as voice notes).
    pub fn open_all(&self, sealed: &[u8]) -> Result<Vec<u8>, CoreError> {
        if sealed.len() as u64 != self.sealed_len() {
            return Err(CoreError::Crypto);
        }
        let cipher = self.cipher();
        let mut out = Vec::with_capacity(usize::try_from(self.size).unwrap_or(0));
        let mut chunk = Vec::with_capacity(self.chunk_size as usize + CHUNK_TAG_LEN);
        for index in 0..self.chunk_count() {
            let (offset, len) = self.chunk_span(index);
            // `offset + len <= sealed_len`, checked above.
            let offset = offset as usize;
            chunk.clear();
            chunk.extend_from_slice(&sealed[offset..offset + len]);
            cipher.open(index, &mut chunk)?;
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    fn cipher(&self) -> cypher_crypto::ChunkCipher {
        cypher_crypto::ChunkCipher::new(
            &FileKey::from_bytes(self.key),
            self.file_id,
            self.chunk_count(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::cipher_for;

    fn desc(size: u64, chunk_size: u32) -> FileDesc {
        FileDesc {
            file_id: FileId([7; 16]),
            name: "v.webm".into(),
            mime: "audio/webm".into(),
            size,
            chunk_size,
            key: [9; 32],
            inline: None,
        }
    }

    fn sealed_file(d: &FileDesc, plain: &[u8]) -> Vec<u8> {
        let cipher = cipher_for(d);
        let mut out = Vec::new();
        for (i, part) in plain.chunks(d.chunk_size as usize).enumerate() {
            let mut buf = part.to_vec();
            cipher.seal(i as u32, &mut buf).unwrap();
            out.extend_from_slice(&buf);
        }
        out
    }

    #[test]
    fn opens_whole_file_and_single_chunks() {
        let plain: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        let d = desc(plain.len() as u64, 4096);
        let file = sealed_file(&d, &plain);
        let key = MediaKey::from_desc(&d);
        assert_eq!(key.sealed_len(), file.len() as u64);
        assert_eq!(key.open_all(&file).unwrap(), plain);

        let (offset, len) = key.chunk_span(2);
        let mut last = file[offset as usize..offset as usize + len].to_vec();
        key.open_chunk(2, &mut last).unwrap();
        assert_eq!(last, plain[8192..]);
    }

    #[test]
    fn rejects_tampering_and_bad_ranges() {
        let plain = vec![5u8; 5000];
        let d = desc(5000, 4096);
        let mut file = sealed_file(&d, &plain);
        let key = MediaKey::from_desc(&d);
        file[10] ^= 1;
        assert!(key.open_all(&file).is_err());
        assert!(key.open_all(&file[1..]).is_err());
        assert!(key.open_chunk(5, &mut vec![0; 20]).is_err());

        assert_eq!(key.chunks_covering(0, 4999), Some(0..=1));
        assert_eq!(key.chunks_covering(4096, 4096), Some(1..=1));
        assert_eq!(key.chunks_covering(0, 5000), None);
        assert_eq!(key.chunks_covering(9, 3), None);
    }
}
