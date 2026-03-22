use cypher_common::{FileId, FileMeta, CHUNK_SIZE};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use cypher_common::Result;

pub struct FileChunker {
    file: File,
    file_id: FileId,
    file_size: u64,
    chunk_count: u32,
    file_hash: Vec<u8>,
}

impl FileChunker {
    /// Opens file, computes total SHA-256 hash, determines chunk count.
    pub async fn new(path: &Path) -> Result<Self> {
        let mut file = File::open(path).await?;
        let metadata = file.metadata().await?;
        let file_size = metadata.len();

        // Compute full-file SHA-256 hash by reading in chunks.
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let file_hash = hasher.finalize().to_vec();

        // Seek back to the beginning for future reads.
        file.seek(std::io::SeekFrom::Start(0)).await?;

        let chunk_count = if file_size == 0 {
            0
        } else {
            file_size.div_ceil(CHUNK_SIZE as u64) as u32
        };

        let file_id = FileId::generate();

        Ok(Self {
            file,
            file_id,
            file_size,
            chunk_count,
            file_hash,
        })
    }

    /// Build a `FileMeta` describing this file.
    pub fn meta(&self, name: String, compressed: bool) -> FileMeta {
        FileMeta {
            file_id: self.file_id.clone(),
            name,
            size: self.file_size,
            chunk_count: self.chunk_count,
            hash: bytes::Bytes::from(self.file_hash.clone()),
            compressed,
        }
    }

    /// Read a single chunk by index, returning `(data, sha256_hash)`.
    ///
    /// Seeks to `index * CHUNK_SIZE` and reads up to `CHUNK_SIZE` bytes.
    pub async fn read_chunk(&mut self, index: u32) -> Result<(Vec<u8>, Vec<u8>)> {
        if index >= self.chunk_count {
            return Err(cypher_common::Error::Transfer(format!(
                "chunk index {} out of range (total {})",
                index, self.chunk_count
            )));
        }

        let offset = index as u64 * CHUNK_SIZE as u64;
        self.file.seek(std::io::SeekFrom::Start(offset)).await?;

        let remaining = self.file_size - offset;
        let to_read = std::cmp::min(remaining, CHUNK_SIZE as u64) as usize;

        let mut buf = vec![0u8; to_read];
        self.file.read_exact(&mut buf).await?;

        let chunk_hash = Sha256::digest(&buf).to_vec();

        Ok((buf, chunk_hash))
    }

    /// Total number of chunks for this file.
    pub fn chunk_count(&self) -> u32 {
        self.chunk_count
    }
}
