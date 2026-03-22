use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cypher_common::{FileId, FileMeta, Result, CHUNK_SIZE};
use sha2::{Digest, Sha256};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// Number of chunks between automatic state saves.
const SAVE_STATE_INTERVAL: u32 = 64;

pub struct FileAssembler {
    file: File,
    #[allow(dead_code)]
    file_id: FileId,
    expected_chunks: u32,
    received: HashSet<u32>,
    expected_hash: Vec<u8>,
    file_size: u64,
    dest_path: PathBuf,
}

/// Compute the path of the `.cypher-state` sidecar file.
fn state_path(dest: &Path) -> PathBuf {
    let mut p = dest.as_os_str().to_os_string();
    p.push(".cypher-state");
    PathBuf::from(p)
}

impl FileAssembler {
    pub async fn new(path: &Path, meta: &FileMeta) -> Result<Self> {
        let file = File::create(path).await?;

        // Pre-allocate the file to the expected size.
        file.set_len(meta.size).await?;

        Ok(Self {
            file,
            file_id: meta.file_id.clone(),
            expected_chunks: meta.chunk_count,
            received: HashSet::new(),
            expected_hash: meta.hash.to_vec(),
            file_size: meta.size,
            dest_path: path.to_path_buf(),
        })
    }

    /// Try to load a previously saved state for a partially received file.
    ///
    /// Returns `None` if no state file exists.
    pub async fn load_state(path: &Path, meta: &FileMeta) -> Result<Option<Self>> {
        let sf = state_path(path);
        if !sf.exists() {
            return Ok(None);
        }
        let data = tokio::fs::read(&sf).await?;
        let mut received = HashSet::new();
        for chunk in data.chunks_exact(4) {
            let idx = u32::from_le_bytes(chunk.try_into().unwrap());
            if idx < meta.chunk_count {
                received.insert(idx);
            }
        }
        // Open the existing file for writing (do not truncate).
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .open(path)
            .await?;
        Ok(Some(Self {
            file,
            file_id: meta.file_id.clone(),
            expected_chunks: meta.chunk_count,
            received,
            expected_hash: meta.hash.to_vec(),
            file_size: meta.size,
            dest_path: path.to_path_buf(),
        }))
    }

    /// Persist received-chunk indices to a sidecar state file.
    pub async fn save_state(&self) -> Result<()> {
        let sf = state_path(&self.dest_path);
        let data: Vec<u8> = self.received.iter().flat_map(|i| i.to_le_bytes()).collect();
        tokio::fs::write(sf, data).await?;
        Ok(())
    }

    /// Remove the sidecar state file (call after successful completion).
    pub async fn cleanup_state(&self) {
        let _ = tokio::fs::remove_file(state_path(&self.dest_path)).await;
    }

    /// Verify chunk hash, seek to the correct position, and write the data.
    pub async fn write_chunk(&mut self, index: u32, data: &[u8], hash: &[u8]) -> Result<()> {
        // Verify chunk hash.
        let computed = Sha256::digest(data);
        if computed.as_slice() != hash {
            return Err(cypher_common::Error::Transfer(format!(
                "chunk {} hash mismatch",
                index
            )));
        }

        if index >= self.expected_chunks {
            return Err(cypher_common::Error::Transfer(format!(
                "chunk index {} out of range (total {})",
                index, self.expected_chunks
            )));
        }

        let offset = index as u64 * CHUNK_SIZE as u64;
        self.file.seek(std::io::SeekFrom::Start(offset)).await?;
        self.file.write_all(data).await?;
        self.file.flush().await?;

        self.received.insert(index);

        // Periodic save every SAVE_STATE_INTERVAL chunks.
        if (self.received.len() as u32).is_multiple_of(SAVE_STATE_INTERVAL) {
            let _ = self.save_state().await;
        }

        Ok(())
    }

    /// Returns `true` when every expected chunk has been received.
    pub fn is_complete(&self) -> bool {
        self.received.len() as u32 == self.expected_chunks
    }

    /// Returns the list of chunk indices that have not yet been received.
    pub fn missing_chunks(&self) -> Vec<u32> {
        (0..self.expected_chunks)
            .filter(|i| !self.received.contains(i))
            .collect()
    }

    /// Progress as a fraction in `[0.0, 1.0]`.
    pub fn progress(&self) -> f64 {
        if self.expected_chunks == 0 {
            return 1.0;
        }
        self.received.len() as f64 / self.expected_chunks as f64
    }

    /// Verify that the full file hash matches the expected hash.
    pub async fn verify(&self) -> Result<bool> {
        // Re-open the file for reading from the start.
        // We need a new handle because `self.file` was opened for writing.
        // Instead, we clone the handle and seek.
        let path_fd = self.file.try_clone().await?;
        let mut reader = path_fd;
        reader.seek(std::io::SeekFrom::Start(0)).await?;

        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK_SIZE];
        let mut bytes_left = self.file_size;

        while bytes_left > 0 {
            let to_read = std::cmp::min(bytes_left, CHUNK_SIZE as u64) as usize;
            let n = reader.read(&mut buf[..to_read]).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            bytes_left -= n as u64;
        }

        let computed = hasher.finalize().to_vec();
        Ok(computed == self.expected_hash)
    }
}
