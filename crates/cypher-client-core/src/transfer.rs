use cypher_common::{FileMeta, Result};
use cypher_transfer::{is_compressible, FileAssembler, FileChunker};
use std::path::Path;
use tracing::debug;

/// High-level transfer coordination helpers.
///
/// These thin wrappers prepare the [`FileChunker`] / [`FileAssembler`] objects
/// that the rest of the client code uses to actually move data.
pub struct TransferManager;

impl TransferManager {
    /// Open `path` for reading, compute its hash and chunk count, and return
    /// the [`FileMeta`] that must be sent to the peer together with the
    /// [`FileChunker`] used to read individual chunks.
    ///
    /// Automatically detects if zstd compression is beneficial by trial-
    /// compressing the first chunk.
    pub async fn prepare_send(path: &Path, file_name: String) -> Result<(FileMeta, FileChunker)> {
        let mut chunker = FileChunker::new(path).await?;

        // Decide compression by trial-compressing the first chunk.
        let compressed = if chunker.chunk_count() > 0 {
            let (sample, _) = chunker.read_chunk(0).await?;
            let result = is_compressible(&sample);
            debug!(file = %file_name, compressed = result, "compression decision");
            result
        } else {
            false
        };

        let meta = chunker.meta(file_name, compressed);
        Ok((meta, chunker))
    }

    /// Create a [`FileAssembler`] that will write incoming chunks to
    /// `dest_path`.
    ///
    /// The file is pre-allocated to `meta.size` bytes immediately.
    pub async fn prepare_recv(dest_path: &Path, meta: &FileMeta) -> Result<FileAssembler> {
        FileAssembler::new(dest_path, meta).await
    }
}
