//! The archive a device of a profile hands a newly linked one: its message
//! rows, their statuses and its voice and video notes, sealed as they are at
//! rest. Every device of a profile holds the same storage key, so the rows
//! need no re-encryption; the transfer seals the archive once more on the
//! way.
//!
//! Layout: `"CYHX" ‖ version`, then records, each led by a tag:
//! - `1` row: `table u8 ‖ key_len u16 ‖ key ‖ value_len u32 ‖ value`;
//! - `2` blob: `file_id[16] ‖ len u64 ‖ bytes`, right after its media row;
//! - `0` end: `rows u32 ‖ blobs u32`, the counts of what came before.
//!
//! Lengths are little-endian. The reader takes the archive in pieces of any
//! size and holds at most one row besides the piece it was given.

use cypher_types::FileId;

use crate::api::{MessageStatus, StoredMessage};
use crate::media::MediaKey;
use crate::store::{Table, Vault, message_key};

/// `"CYHX"` and the format version.
const HEADER: [u8; 5] = *b"CYHX\x01";
const HEADER_LEN: usize = HEADER.len();

const TAG_END: u8 = 0;
const TAG_ROW: u8 = 1;
const TAG_BLOB: u8 = 2;

/// Longest key a history row may have (a message key is 56 bytes).
pub const MAX_KEY_LEN: usize = 64;
/// Largest sealed value a history row may have.
pub const MAX_VALUE_LEN: usize = 256 * 1024;

const ROW_HEAD_LEN: usize = 1 + 1 + 2;
const BLOB_HEAD_LEN: usize = 1 + 16 + 8;
const END_LEN: usize = 1 + 4 + 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ArchiveError {
    #[error("not a history archive, or a broken one")]
    Malformed,
    #[error("the history archive ended early")]
    Truncated,
}

/// The tables a history archive carries.
fn carried(table: u8) -> Option<Table> {
    match table {
        t if t == Table::Messages as u8 => Some(Table::Messages),
        t if t == Table::MessageStatus as u8 => Some(Table::MessageStatus),
        t if t == Table::Media as u8 => Some(Table::Media),
        _ => None,
    }
}

/// Writes an archive into buffers the caller flushes, counting what it
/// wrote for the closing record.
#[derive(Debug, Default)]
pub struct Writer {
    rows: u32,
    blobs: u32,
}

impl Writer {
    pub fn header(out: &mut Vec<u8>) {
        out.extend_from_slice(&HEADER);
    }

    /// A sealed row of `table`, which must be one the archive carries. A
    /// media row must be followed by its blob.
    pub fn row(
        &mut self,
        out: &mut Vec<u8>,
        table: Table,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), ArchiveError> {
        let key_len = u16::try_from(key.len()).map_err(|_| ArchiveError::Malformed)?;
        let value_len = u32::try_from(value.len()).map_err(|_| ArchiveError::Malformed)?;
        if carried(table as u8).is_none() || key.len() > MAX_KEY_LEN || value.len() > MAX_VALUE_LEN
        {
            return Err(ArchiveError::Malformed);
        }
        out.reserve(ROW_HEAD_LEN + key.len() + 4 + value.len());
        out.extend_from_slice(&[TAG_ROW, table as u8]);
        out.extend_from_slice(&key_len.to_le_bytes());
        out.extend_from_slice(key);
        out.extend_from_slice(&value_len.to_le_bytes());
        out.extend_from_slice(value);
        self.rows = self.rows.saturating_add(1);
        Ok(())
    }

    /// Starts the blob of the media row just written: `len` bytes of it
    /// follow, written by the caller as they are.
    pub fn blob(&mut self, out: &mut Vec<u8>, file_id: &FileId, len: u64) {
        out.push(TAG_BLOB);
        out.extend_from_slice(file_id.as_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        self.blobs = self.blobs.saturating_add(1);
    }

    pub fn end(&self, out: &mut Vec<u8>) {
        out.push(TAG_END);
        out.extend_from_slice(&self.rows.to_le_bytes());
        out.extend_from_slice(&self.blobs.to_le_bytes());
    }
}

/// What the reader found next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item<'a> {
    Row {
        table: Table,
        key: &'a [u8],
        value: &'a [u8],
    },
    /// The blob of the media row just read: `len` bytes follow as
    /// [`Item::BlobData`].
    BlobStart {
        file_id: FileId,
        len: u64,
    },
    BlobData(&'a [u8]),
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Header,
    Records,
    /// A media row was read: its blob comes next.
    AfterMedia(FileId),
    Blob {
        left: u64,
    },
    Done,
}

/// Reads an archive pushed to it in pieces.
#[derive(Debug)]
pub struct Reader {
    buf: Vec<u8>,
    /// Bytes of `buf` already handed out.
    start: usize,
    stage: Stage,
    rows: u32,
    blobs: u32,
}

impl Default for Reader {
    fn default() -> Self {
        Self::new()
    }
}

/// `N` bytes at `at`, once they arrived.
fn fixed<const N: usize>(buf: &[u8], at: usize) -> Option<[u8; N]> {
    buf.get(at..)?.first_chunk::<N>().copied()
}

/// The bytes `at..at + len`, once they arrived.
fn span(buf: &[u8], at: usize, len: usize) -> Option<&[u8]> {
    buf.get(at..at.checked_add(len)?)
}

impl Reader {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            start: 0,
            stage: Stage::Header,
            rows: 0,
            blobs: 0,
        }
    }

    /// Adds the next piece of the archive. What [`Reader::next_item`]
    /// handed out before is dropped here.
    pub fn push(&mut self, piece: &[u8]) {
        self.buf.drain(..self.start);
        self.start = 0;
        self.buf.extend_from_slice(piece);
    }

    /// The next whole item in what was pushed so far, or `None` until more
    /// is pushed.
    pub fn next_item(&mut self) -> Result<Option<Item<'_>>, ArchiveError> {
        loop {
            match self.stage {
                Stage::Header => {
                    let Some(header) = fixed::<HEADER_LEN>(&self.buf, self.start) else {
                        return Ok(None);
                    };
                    if header != HEADER {
                        return Err(ArchiveError::Malformed);
                    }
                    self.start += HEADER_LEN;
                    self.stage = Stage::Records;
                }
                Stage::Blob { left: 0 } => self.stage = Stage::Records,
                Stage::Blob { left } => return Ok(self.blob_data(left)),
                Stage::Done if self.start == self.buf.len() => return Ok(None),
                Stage::Done => return Err(ArchiveError::Malformed),
                Stage::Records | Stage::AfterMedia(_) => {
                    return match self.buf.get(self.start) {
                        None => Ok(None),
                        Some(&TAG_ROW) => self.row(),
                        Some(&TAG_BLOB) => self.blob_start(),
                        Some(&TAG_END) => self.end(),
                        Some(_) => Err(ArchiveError::Malformed),
                    };
                }
            }
        }
    }

    fn blob_data(&mut self, left: u64) -> Option<Item<'_>> {
        let at = self.start;
        let avail = self.buf.len() - at;
        let take = usize::try_from(left).map_or(avail, |left| left.min(avail));
        if take == 0 {
            return None;
        }
        self.start += take;
        self.stage = Stage::Blob {
            left: left - u64::try_from(take).unwrap_or(left),
        };
        span(&self.buf, at, take).map(Item::BlobData)
    }

    fn row(&mut self) -> Result<Option<Item<'_>>, ArchiveError> {
        if matches!(self.stage, Stage::AfterMedia(_)) {
            return Err(ArchiveError::Malformed);
        }
        let at = self.start;
        let Some([_, table, k0, k1]) = fixed::<ROW_HEAD_LEN>(&self.buf, at) else {
            return Ok(None);
        };
        let table = carried(table).ok_or(ArchiveError::Malformed)?;
        let key_len = usize::from(u16::from_le_bytes([k0, k1]));
        if key_len > MAX_KEY_LEN || (table == Table::Media && key_len != 16) {
            return Err(ArchiveError::Malformed);
        }
        let key_at = at + ROW_HEAD_LEN;
        let len_at = key_at + key_len;
        let Some(len) = fixed::<4>(&self.buf, len_at) else {
            return Ok(None);
        };
        let value_len =
            usize::try_from(u32::from_le_bytes(len)).map_err(|_| ArchiveError::Malformed)?;
        if value_len > MAX_VALUE_LEN {
            return Err(ArchiveError::Malformed);
        }
        let value_at = len_at + 4;
        let end = value_at + value_len;
        if self.buf.len() < end {
            return Ok(None);
        }
        if table == Table::Media {
            let id = fixed::<16>(&self.buf, key_at).ok_or(ArchiveError::Malformed)?;
            self.stage = Stage::AfterMedia(FileId(id));
        }
        self.start = end;
        self.rows = self.rows.saturating_add(1);
        let (Some(key), Some(value)) = (
            span(&self.buf, key_at, key_len),
            span(&self.buf, value_at, value_len),
        ) else {
            return Err(ArchiveError::Malformed);
        };
        Ok(Some(Item::Row { table, key, value }))
    }

    fn blob_start(&mut self) -> Result<Option<Item<'_>>, ArchiveError> {
        let Stage::AfterMedia(expected) = self.stage else {
            return Err(ArchiveError::Malformed);
        };
        let at = self.start;
        let (Some(id), Some(len)) = (
            fixed::<16>(&self.buf, at + 1),
            fixed::<8>(&self.buf, at + 17),
        ) else {
            return Ok(None);
        };
        if FileId(id) != expected {
            return Err(ArchiveError::Malformed);
        }
        let len = u64::from_le_bytes(len);
        self.start += BLOB_HEAD_LEN;
        self.stage = Stage::Blob { left: len };
        self.blobs = self.blobs.saturating_add(1);
        Ok(Some(Item::BlobStart {
            file_id: expected,
            len,
        }))
    }

    fn end(&mut self) -> Result<Option<Item<'_>>, ArchiveError> {
        if matches!(self.stage, Stage::AfterMedia(_)) {
            return Err(ArchiveError::Malformed);
        }
        let at = self.start;
        let (Some(rows), Some(blobs)) =
            (fixed::<4>(&self.buf, at + 1), fixed::<4>(&self.buf, at + 5))
        else {
            return Ok(None);
        };
        if (u32::from_le_bytes(rows), u32::from_le_bytes(blobs)) != (self.rows, self.blobs) {
            return Err(ArchiveError::Malformed);
        }
        self.start += END_LEN;
        self.stage = Stage::Done;
        Ok(Some(Item::End))
    }

    /// The archive is over: it must have been whole.
    pub fn finish(&self) -> Result<(), ArchiveError> {
        match self.stage {
            Stage::Done if self.start == self.buf.len() => Ok(()),
            Stage::Done => Err(ArchiveError::Malformed),
            Stage::Header | Stage::Records | Stage::AfterMedia(_) | Stage::Blob { .. } => {
                Err(ArchiveError::Truncated)
            }
        }
    }
}

/// Whether a row from an archive is one this profile sealed, under the key
/// it is stored at: the importer keeps only these.
pub fn check_row(vault: &Vault, table: Table, key: &[u8], value: &[u8]) -> bool {
    match table {
        Table::Messages => vault
            .open::<StoredMessage>(table, key, value)
            .is_ok_and(|m| message_key(&m.peer, m.sent_at_ms, &m.msg_id) == key),
        Table::MessageStatus => {
            key.len() == 16 && vault.open::<MessageStatus>(table, key, value).is_ok()
        }
        Table::Media => vault
            .open::<MediaKey>(table, key, value)
            .is_ok_and(|m| m.file_id.as_bytes() == key),
        Table::Meta | Table::Peers | Table::Outbox | Table::Transfers | Table::Sessions => false,
    }
}

#[cfg(test)]
mod tests {
    use cypher_types::{MsgId, PeerId};
    use rand_core::OsRng;

    use super::*;
    use crate::api::Content;
    use crate::envelope::FileDesc;

    fn message(n: u8) -> StoredMessage {
        StoredMessage {
            msg_id: MsgId([n; 16]),
            peer: PeerId([7; 32]),
            outgoing: n.is_multiple_of(2),
            sent_at_ms: u64::from(n) * 1000,
            status: MessageStatus::Delivered,
            content: Content::Text {
                text: format!("message {n}"),
                reply_to: None,
            },
        }
    }

    /// An archive with two messages, a status and one media note.
    fn archive(vault: &Vault) -> (Vec<u8>, Vec<u8>) {
        let mut out = Vec::new();
        let mut w = Writer::default();
        Writer::header(&mut out);
        let media = MediaKey::from_desc(&FileDesc {
            file_id: FileId([9; 16]),
            name: "voice".into(),
            mime: "audio/ogg".into(),
            size: 10,
            chunk_size: 64 * 1024,
            key: [3; 32],
            inline: None,
        });
        let blob = vec![0xab; usize::try_from(media.sealed_len()).unwrap()];
        let sealed = vault.seal(Table::Media, &[9; 16], &media, &mut OsRng);
        w.row(&mut out, Table::Media, &[9; 16], &sealed).unwrap();
        w.blob(&mut out, &media.file_id, blob.len() as u64);
        out.extend_from_slice(&blob);
        for n in 1..=2 {
            let m = message(n);
            let key = message_key(&m.peer, m.sent_at_ms, &m.msg_id);
            let sealed = vault.seal(Table::Messages, &key, &m, &mut OsRng);
            w.row(&mut out, Table::Messages, &key, &sealed).unwrap();
        }
        let sealed = vault.seal(
            Table::MessageStatus,
            &[1; 16],
            &MessageStatus::Read,
            &mut OsRng,
        );
        w.row(&mut out, Table::MessageStatus, &[1; 16], &sealed)
            .unwrap();
        w.end(&mut out);
        (out, blob)
    }

    type Row = (Table, Vec<u8>, Vec<u8>);

    /// Everything the reader yields, fed `step` bytes at a time.
    fn read_all(bytes: &[u8], step: usize) -> Result<(Vec<Row>, Vec<u8>), ArchiveError> {
        let mut r = Reader::new();
        let (mut rows, mut blob) = (Vec::new(), Vec::new());
        for piece in bytes.chunks(step) {
            r.push(piece);
            while let Some(item) = r.next_item()? {
                match item {
                    Item::Row { table, key, value } => {
                        rows.push((table, key.to_vec(), value.to_vec()));
                    }
                    Item::BlobStart { .. } | Item::End => {}
                    Item::BlobData(data) => blob.extend_from_slice(data),
                }
            }
        }
        r.finish()?;
        Ok((rows, blob))
    }

    #[test]
    fn an_archive_reads_back_whatever_the_pieces() {
        let vault = Vault::new([5; 32]);
        let (bytes, blob) = archive(&vault);
        for step in [1, 3, 64, 1000, bytes.len()] {
            let (rows, read_blob) = read_all(&bytes, step).unwrap();
            assert_eq!(rows.len(), 4);
            assert_eq!(read_blob, blob);
            for (table, key, value) in &rows {
                assert!(
                    check_row(&vault, *table, key, value),
                    "{table:?} row checks out"
                );
            }
        }
    }

    #[test]
    fn a_cut_archive_is_truncated() {
        let vault = Vault::new([5; 32]);
        let (bytes, _) = archive(&vault);
        for cut in [0, 3, 5, 40, bytes.len() - 1] {
            assert_eq!(
                read_all(&bytes[..cut], 7).unwrap_err(),
                ArchiveError::Truncated
            );
        }
    }

    #[test]
    fn broken_archives_are_refused() {
        let vault = Vault::new([5; 32]);
        let (bytes, _) = archive(&vault);
        let malformed = |bytes: &[u8]| read_all(bytes, 16).unwrap_err() == ArchiveError::Malformed;

        let mut magic = bytes.clone();
        magic[0] = b'X';
        assert!(malformed(&magic));
        let mut version = bytes.clone();
        version[4] = 2;
        assert!(malformed(&version));
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(malformed(&trailing));
        let mut tag = bytes.clone();
        tag[HEADER_LEN] = 9;
        assert!(malformed(&tag));
        let mut table = bytes.clone();
        table[HEADER_LEN + 1] = Table::Sessions as u8;
        assert!(malformed(&table));
        let mut counts = bytes;
        let last = counts.len() - 1;
        counts[last] = 7;
        assert!(malformed(&counts));
    }

    #[test]
    fn oversized_lengths_are_refused_before_waiting_for_them() {
        let mut out = Vec::new();
        Writer::header(&mut out);
        out.extend_from_slice(&[TAG_ROW, Table::Messages as u8]);
        out.extend_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(read_all(&out, 4).unwrap_err(), ArchiveError::Malformed);

        let mut out = Vec::new();
        Writer::header(&mut out);
        out.extend_from_slice(&[TAG_ROW, Table::Messages as u8, 1, 0, 0]);
        out.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(read_all(&out, 4).unwrap_err(), ArchiveError::Malformed);

        let mut w = Writer::default();
        let long = vec![0; MAX_KEY_LEN + 1];
        assert_eq!(
            w.row(&mut out, Table::Messages, &long, b"v"),
            Err(ArchiveError::Malformed)
        );
        assert_eq!(
            w.row(&mut out, Table::Peers, b"k", b"v"),
            Err(ArchiveError::Malformed)
        );
    }

    #[test]
    fn a_blob_comes_only_right_after_its_media_row() {
        let mut out = Vec::new();
        let mut w = Writer::default();
        Writer::header(&mut out);
        w.blob(&mut out, &FileId([1; 16]), 0);
        w.end(&mut out);
        assert_eq!(read_all(&out, 8).unwrap_err(), ArchiveError::Malformed);

        let mut out = Vec::new();
        let mut w = Writer::default();
        Writer::header(&mut out);
        w.row(&mut out, Table::Media, &[1; 16], b"sealed").unwrap();
        w.blob(&mut out, &FileId([2; 16]), 0);
        w.end(&mut out);
        assert_eq!(read_all(&out, 8).unwrap_err(), ArchiveError::Malformed);

        let mut out = Vec::new();
        let mut w = Writer::default();
        Writer::header(&mut out);
        w.row(&mut out, Table::Media, &[1; 16], b"sealed").unwrap();
        w.end(&mut out);
        assert_eq!(read_all(&out, 8).unwrap_err(), ArchiveError::Malformed);
    }

    #[test]
    fn rows_sealed_elsewhere_or_moved_do_not_check_out() {
        let vault = Vault::new([5; 32]);
        let other = Vault::new([6; 32]);
        let m = message(1);
        let key = message_key(&m.peer, m.sent_at_ms, &m.msg_id);
        let sealed = other.seal(Table::Messages, &key, &m, &mut OsRng);
        assert!(!check_row(&vault, Table::Messages, &key, &sealed));

        // Sealed under a key that does not match what it says inside.
        let wrong_key = message_key(&m.peer, m.sent_at_ms + 1, &m.msg_id);
        let sealed = vault.seal(Table::Messages, &wrong_key, &m, &mut OsRng);
        assert!(!check_row(&vault, Table::Messages, &wrong_key, &sealed));

        let sealed = vault.seal(
            Table::MessageStatus,
            &[1; 15],
            &MessageStatus::Read,
            &mut OsRng,
        );
        assert!(!check_row(&vault, Table::MessageStatus, &[1; 15], &sealed));
        assert!(!check_row(&vault, Table::Peers, &[1; 32], &sealed));
    }
}
