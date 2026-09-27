//! Voice and video notes: sending recordings held in memory and ranged,
//! decrypt-on-read playback of the sealed copies both sides keep.

use std::fs::File;
use std::path::Path;

use cypher_core::MediaKey;

use crate::ClientError;
use crate::files::read_exact_at;

/// Largest plaintext slice one ranged read returns; players ask again for
/// the rest, so memory stays bounded however large the note is.
pub const MAX_RANGE_LEN: u64 = 1 << 20;

/// Plaintext bytes `start..=end` of a `total`-byte media file.
#[derive(Debug)]
pub struct MediaSlice {
    pub mime: String,
    pub total: u64,
    pub start: u64,
    pub end: u64,
    pub bytes: Vec<u8>,
}

/// Decrypts the chunks covering `start..=end` (clamped to the file and to
/// [`MAX_RANGE_LEN`]). Blocking: run it on a blocking thread.
pub(crate) fn read_range(
    path: &Path,
    key: &MediaKey,
    start: u64,
    end: Option<u64>,
) -> Result<MediaSlice, ClientError> {
    let last = key.size.checked_sub(1).ok_or(ClientError::InvalidInput)?;
    let end = end
        .unwrap_or(last)
        .min(last)
        .min(start.saturating_add(MAX_RANGE_LEN - 1));
    let chunks = key
        .chunks_covering(start, end)
        .ok_or(ClientError::InvalidInput)?;
    let file = File::open(path)?;
    let chunk_size = u64::from(key.chunk_size);
    let first_offset = u64::from(*chunks.start()) * chunk_size;

    let mut plain = Vec::with_capacity((end - start + 1) as usize);
    let mut buf = Vec::with_capacity(key.chunk_size as usize + 16);
    for index in chunks {
        let (offset, len) = key.chunk_span(index);
        buf.resize(len, 0);
        read_exact_at(&file, &mut buf, offset)?;
        key.open_chunk(index, &mut buf)?;
        plain.extend_from_slice(&buf);
    }
    // Both bounds lie inside the decrypted chunks, and the slice is at most
    // MAX_RANGE_LEN long.
    let from = (start - first_offset) as usize;
    let to = (end - first_offset) as usize;
    plain.truncate(to + 1);
    plain.drain(..from);
    Ok(MediaSlice {
        mime: key.mime.clone(),
        total: key.size,
        start,
        end,
        bytes: plain,
    })
}
