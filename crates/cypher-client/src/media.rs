//! Voice and video notes: sending recordings held in memory and ranged,
//! decrypt-on-read playback of the sealed copies both sides keep.

use std::fs::File;
use std::io;
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
///
/// A note still being received yields the leading chunks that are here: a
/// chunk not yet written fails to authenticate, so nothing unverified is
/// played. [`ClientError::NotReady`] when not even the first one is.
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

    let span = usize::try_from(end - start + 1).map_err(|_| ClientError::InvalidInput)?;
    let mut plain = Vec::with_capacity(span);
    let mut buf = Vec::with_capacity(key.chunk_size as usize + 16);
    for index in chunks {
        let (offset, len) = key.chunk_span(index);
        buf.resize(len, 0);
        match read_exact_at(&file, &mut buf, offset) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        if key.open_chunk(index, &mut buf).is_err() {
            break;
        }
        plain.extend_from_slice(&buf);
    }
    // `start` lies inside the first chunk; `end` is cut to what decrypted.
    let from = usize::try_from(start - first_offset).map_err(|_| ClientError::InvalidInput)?;
    if plain.len() <= from {
        return Err(ClientError::NotReady);
    }
    let span = span.min(plain.len() - from);
    let end = start + span as u64 - 1;
    plain.truncate(from + span);
    plain.drain(..from);
    Ok(MediaSlice {
        mime: key.mime.clone(),
        total: key.size,
        start,
        end,
        bytes: plain,
    })
}

#[cfg(test)]
mod tests {
    use cypher_crypto::{ChunkCipher, FileKey};
    use cypher_types::FileId;
    use serde::Serialize;

    use super::*;

    const CHUNK: u32 = 1000;
    const SIZE: u64 = 3500;

    /// `MediaKey`'s stored form, to build one the way the vault reads it.
    #[derive(Serialize)]
    struct StoredKey<'a> {
        file_id: FileId,
        mime: &'a str,
        size: u64,
        chunk_size: u32,
        key: [u8; 32],
    }

    fn key() -> MediaKey {
        let stored = StoredKey {
            file_id: FileId([3; 16]),
            mime: "video/webm",
            size: SIZE,
            chunk_size: CHUNK,
            key: [8; 32],
        };
        postcard::from_bytes(&postcard::to_allocvec(&stored).unwrap()).unwrap()
    }

    fn plain() -> Vec<u8> {
        (0..SIZE).map(|i| (i % 251) as u8).collect()
    }

    /// The sealed file as the sink writes it, holding only chunks `0..held`.
    fn sealed(held: u32) -> Vec<u8> {
        let cipher = ChunkCipher::new(&FileKey::from_bytes([8; 32]), FileId([3; 16]), 4);
        let mut out = Vec::new();
        for (index, part) in (0..held).zip(plain().chunks(CHUNK as usize)) {
            let mut buf = part.to_vec();
            cipher.seal(index, &mut buf).unwrap();
            out.extend_from_slice(&buf);
        }
        out
    }

    fn read(file: &[u8], start: u64, end: Option<u64>) -> Result<MediaSlice, ClientError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note");
        std::fs::write(&path, file).unwrap();
        read_range(&path, &key(), start, end)
    }

    #[test]
    fn a_whole_note_reads_in_any_range() {
        let file = sealed(4);
        let all = read(&file, 0, None).unwrap();
        assert_eq!((all.start, all.end, all.total), (0, SIZE - 1, SIZE));
        assert_eq!(all.bytes, plain());
        let middle = read(&file, 990, Some(2010)).unwrap();
        assert_eq!(middle.bytes, plain()[990..=2010]);
        assert_eq!(middle.mime, "video/webm");
        read(&file, SIZE, None).unwrap_err();
    }

    #[test]
    fn a_note_being_received_reads_as_far_as_it_came() {
        // Two chunks written, the rest not yet: the file simply ends.
        let partial = sealed(2);
        let head = read(&partial, 500, None).unwrap();
        assert_eq!((head.start, head.end, head.total), (500, 1999, SIZE));
        assert_eq!(head.bytes, plain()[500..2000]);
        assert!(matches!(
            read(&partial, 2000, None),
            Err(ClientError::NotReady)
        ));

        // A sink that sized the file up front leaves zeros, which never open.
        let mut zeroed = partial;
        zeroed.resize(usize::try_from(key().sealed_len()).unwrap(), 0);
        assert_eq!(read(&zeroed, 0, None).unwrap().end, 1999);
        assert!(matches!(
            read(&zeroed, 3000, None),
            Err(ClientError::NotReady)
        ));
        assert!(matches!(read(&[], 0, None), Err(ClientError::NotReady)));
    }
}
