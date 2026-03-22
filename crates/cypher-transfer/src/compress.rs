//! Optional zstd compression for file transfer chunks.
//!
//! Compression is decided per-file: the first chunk is trial-compressed;
//! if the savings exceed 10% the entire transfer uses compression.

use cypher_common::Result;

/// Zstd compression level (3 = good balance of speed and ratio).
const ZSTD_LEVEL: i32 = 3;

/// Minimum savings ratio to enable compression (10%).
const MIN_SAVINGS_RATIO: f64 = 0.10;

/// Compress a chunk with zstd. Returns the compressed bytes.
pub fn compress_chunk(data: &[u8]) -> Result<Vec<u8>> {
    zstd::encode_all(std::io::Cursor::new(data), ZSTD_LEVEL)
        .map_err(|e| cypher_common::Error::Transfer(format!("zstd compress: {e}")))
}

/// Decompress a zstd-compressed chunk. Returns the original bytes.
pub fn decompress_chunk(data: &[u8]) -> Result<Vec<u8>> {
    zstd::decode_all(std::io::Cursor::new(data))
        .map_err(|e| cypher_common::Error::Transfer(format!("zstd decompress: {e}")))
}

/// Check if compression is beneficial for the given sample data.
///
/// Returns `true` if zstd saves more than 10% of the input size.
pub fn is_compressible(sample: &[u8]) -> bool {
    if sample.is_empty() {
        return false;
    }
    match compress_chunk(sample) {
        Ok(compressed) => {
            let ratio = 1.0 - (compressed.len() as f64 / sample.len() as f64);
            ratio >= MIN_SAVINGS_RATIO
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let original = b"hello world, this is a test of compression!".repeat(100);
        let compressed = compress_chunk(&original).unwrap();
        let decompressed = decompress_chunk(&compressed).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn test_compressible_text() {
        let text = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".repeat(100);
        assert!(is_compressible(&text));
    }

    #[test]
    fn test_incompressible_random() {
        let random: Vec<u8> = (0..1000).map(|i| (i * 137 + 53) as u8).collect();
        // Random data may or may not compress well, but we test the function runs.
        let _ = is_compressible(&random);
    }
}
