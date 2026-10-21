//! Builds and takes in the message history archive a newly linked device
//! gets ([`cypher_core::history`]). Both run on a blocking thread, reading
//! and writing the store a page at a time and the files as streams, so a
//! long history never sits in memory.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use cypher_core::history::{self, Item, Reader, Writer};
use cypher_core::{Table, Vault};
use cypher_types::FileId;

use crate::store::{Op, Store};

/// Rows read from or written to the store at a time.
const PAGE: usize = 256;
/// Bytes of the archive read at a time while taking it in.
const READ_LEN: usize = 64 * 1024;

fn other(e: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::other(e)
}

/// Every row of `table` in key order, a page at a time.
fn each_row(
    store: &Store,
    table: Table,
    mut row: impl FnMut(&[u8], &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let mut after = Vec::new();
    loop {
        let page = store
            .blocking_after(table.into(), after, PAGE)
            .map_err(other)?;
        for (key, value) in &page {
            row(key, value)?;
        }
        let full = page.len() == PAGE;
        match page.into_iter().last() {
            Some((key, _)) if full => after = key,
            _ => return Ok(()),
        }
    }
}

/// Writes the archive at `path`: every message and status row, and each
/// voice or video note whose sealed copy is whole and not still `partial`.
/// Returns its size.
pub(crate) fn build(
    store: &Store,
    vault: &Vault,
    media_path: impl Fn(&FileId) -> PathBuf,
    path: &Path,
    partial: &[FileId],
) -> io::Result<u64> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut out = BufWriter::new(File::create(&tmp)?);
    let mut w = Writer::default();
    let mut buf = Vec::new();
    Writer::header(&mut buf);

    each_row(store, Table::Media, |key, value| {
        let Ok(id) = <[u8; 16]>::try_from(key) else {
            return Ok(());
        };
        let file_id = FileId(id);
        let Ok(media) = vault.open_media(&file_id, value) else {
            return Ok(());
        };
        let len = media.sealed_len();
        let note = File::open(media_path(&file_id))
            .ok()
            .filter(|f| f.metadata().is_ok_and(|m| m.len() == len));
        let Some(note) = note else {
            return Ok(());
        };
        if partial.contains(&file_id) || w.row(&mut buf, Table::Media, key, value).is_err() {
            return Ok(());
        }
        w.blob(&mut buf, &file_id, len);
        out.write_all(&buf)?;
        buf.clear();
        let copied = io::copy(&mut note.take(len), &mut out)?;
        if copied == len {
            Ok(())
        } else {
            Err(io::ErrorKind::UnexpectedEof.into())
        }
    })?;
    for table in [Table::Messages, Table::MessageStatus] {
        each_row(store, table, |key, value| {
            // A row too big for the archive stays behind.
            if w.row(&mut buf, table, key, value).is_ok() && buf.len() >= READ_LEN {
                out.write_all(&buf)?;
                buf.clear();
            }
            Ok(())
        })?;
    }
    w.end(&mut buf);
    out.write_all(&buf)?;
    let file = out.into_inner().map_err(io::IntoInnerError::into_error)?;
    file.sync_all()?;
    let size = file.metadata()?.len();
    drop(file);
    fs::rename(&tmp, path)?;
    Ok(size)
}

/// A voice or video note read from the archive, written beside where it
/// goes until it is whole.
struct Note {
    file_id: FileId,
    key: Vec<u8>,
    value: Vec<u8>,
    /// Bytes of it still to come, once its blob started.
    left: Option<u64>,
    /// Its length is the one its key says.
    fits: bool,
    /// Where it is being written; `None` when this device keeps it already.
    file: Option<(File, PathBuf)>,
}

/// Takes in the archive at `path`: the rows this store does not have yet,
/// each only if this profile sealed it, and the notes of the media rows.
pub(crate) fn import(
    store: &Store,
    vault: &Vault,
    media_path: impl Fn(&FileId) -> PathBuf,
    path: &Path,
) -> io::Result<()> {
    let mut archive = File::open(path)?;
    let mut reader = Reader::new();
    let mut importer = Importer {
        store,
        vault,
        media_path,
        ops: Vec::with_capacity(PAGE),
        note: None,
    };
    let mut piece = vec![0u8; READ_LEN];
    loop {
        let n = archive.read(&mut piece)?;
        if n == 0 {
            break;
        }
        reader.push(piece.get(..n).unwrap_or_default());
        while let Some(item) = reader.next_item().map_err(other)? {
            importer.take(item)?;
        }
    }
    reader.finish().map_err(other)?;
    store.blocking_apply(importer.ops).map_err(other)
}

/// What an import has read so far and not yet stored.
struct Importer<'a, M> {
    store: &'a Store,
    vault: &'a Vault,
    media_path: M,
    ops: Vec<Op>,
    note: Option<Note>,
}

impl<M: Fn(&FileId) -> PathBuf> Importer<'_, M> {
    fn take(&mut self, item: Item<'_>) -> io::Result<()> {
        match item {
            Item::Row { table, key, value } => self.row(table, key, value)?,
            Item::BlobStart { file_id, len } => self.blob_start(file_id, len)?,
            Item::BlobData(data) => self.blob_data(data)?,
            Item::End => {}
        }
        if self.note.as_ref().is_some_and(|n| n.left == Some(0))
            && let Some(note) = self.note.take()
        {
            self.finish_note(note)?;
        }
        if self.ops.len() >= PAGE {
            let ops = std::mem::take(&mut self.ops);
            self.store.blocking_apply(ops).map_err(other)?;
        }
        Ok(())
    }

    fn row(&mut self, table: Table, key: &[u8], value: &[u8]) -> io::Result<()> {
        if !history::check_row(self.vault, table, key, value) {
            return Ok(());
        }
        if table == Table::Media {
            let id = <[u8; 16]>::try_from(key).map_err(other)?;
            self.note = Some(Note {
                file_id: FileId(id),
                key: key.to_vec(),
                value: value.to_vec(),
                left: None,
                fits: false,
                file: None,
            });
        } else {
            let op = Op::PutIfAbsent(table.into(), key.to_vec(), value.to_vec());
            self.ops.push(op);
        }
        Ok(())
    }

    /// Where the note is written, unless this device has it already or it
    /// does not fit its key.
    fn blob_start(&mut self, file_id: FileId, len: u64) -> io::Result<()> {
        let Some(note) = self.note.as_mut().filter(|n| n.file_id == file_id) else {
            return Ok(());
        };
        note.fits = self
            .vault
            .open_media(&note.file_id, &note.value)
            .is_ok_and(|m| m.sealed_len() == len);
        note.left = Some(len);
        let target = (self.media_path)(&note.file_id);
        if !note.fits || target.exists() {
            return Ok(());
        }
        if let Some(dir) = target.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = target.with_extension("tmp");
        note.file = Some((File::create(&tmp)?, tmp));
        Ok(())
    }

    fn blob_data(&mut self, data: &[u8]) -> io::Result<()> {
        let Some(note) = self.note.as_mut() else {
            return Ok(());
        };
        if let Some((file, _)) = &mut note.file {
            file.write_all(data)?;
        }
        note.left = note.left.map(|left| left.saturating_sub(data.len() as u64));
        Ok(())
    }

    /// The note is whole: it takes its place, and only then its row.
    fn finish_note(&mut self, note: Note) -> io::Result<()> {
        if let Some((file, tmp)) = note.file {
            file.sync_all()?;
            drop(file);
            fs::rename(tmp, (self.media_path)(&note.file_id))?;
        }
        if note.fits {
            let op = Op::PutIfAbsent(Table::Media.into(), note.key, note.value);
            self.ops.push(op);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use cypher_core::envelope::FileDesc;
    use cypher_core::{Content, MediaKey, MessageStatus, StoredMessage, message_key};
    use cypher_types::{MsgId, PeerId};
    use rand::rngs::OsRng;

    use super::*;

    /// One device of a profile: its store and where it keeps notes.
    struct Device {
        store: Store,
        media: PathBuf,
        _dir: tempfile::TempDir,
    }

    impl Device {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            Self {
                store: Store::open(&dir.path().join("db")).unwrap(),
                media: dir.path().join("media"),
                _dir: dir,
            }
        }

        fn media_path(&self) -> impl Fn(&FileId) -> PathBuf + use<> {
            let media = self.media.clone();
            move |id: &FileId| media.join(id.to_hex())
        }

        fn put(&self, table: Table, key: Vec<u8>, value: Vec<u8>) {
            self.store
                .blocking_apply(vec![Op::Put(table.into(), key, value)])
                .unwrap();
        }

        fn rows(&self, table: Table) -> Vec<(Vec<u8>, Vec<u8>)> {
            self.store
                .blocking_after(table.into(), Vec::new(), 1000)
                .unwrap()
        }
    }

    fn text(n: u8) -> StoredMessage {
        StoredMessage {
            msg_id: MsgId([n; 16]),
            peer: PeerId([4; 32]),
            outgoing: false,
            sent_at_ms: u64::from(n),
            status: MessageStatus::Delivered,
            content: Content::Text {
                text: format!("text {n}"),
                reply_to: None,
            },
        }
    }

    fn put_message(d: &Device, vault: &Vault, m: &StoredMessage) -> Vec<u8> {
        let key = message_key(&m.peer, m.sent_at_ms, &m.msg_id);
        d.put(
            Table::Messages,
            key.clone(),
            vault.seal(Table::Messages, &key, m, &mut OsRng),
        );
        key
    }

    /// A voice note of `size` bytes kept sealed on `d`: its id and copy.
    fn put_note(d: &Device, vault: &Vault, id: u8, size: u64) -> (FileId, Vec<u8>) {
        let file_id = FileId([id; 16]);
        let key = MediaKey::from_desc(&FileDesc {
            file_id,
            name: "voice".into(),
            mime: "audio/ogg".into(),
            size,
            chunk_size: 64 * 1024,
            key: [id; 32],
            inline: None,
        });
        let sealed = vault.seal(Table::Media, file_id.as_bytes(), &key, &mut OsRng);
        d.put(Table::Media, file_id.to_vec(), sealed);
        let copy = vec![id; usize::try_from(key.sealed_len()).unwrap()];
        fs::create_dir_all(&d.media).unwrap();
        fs::write(d.media_path()(&file_id), &copy).unwrap();
        (file_id, copy)
    }

    /// Builds on `from`, takes in on `to`: both on a thread that may block.
    fn hand_over(from: &Device, to: &Device, vault: &Vault, partial: &[FileId]) -> io::Result<()> {
        let archive = from.media.with_file_name("history.bin");
        build(&from.store, vault, from.media_path(), &archive, partial)?;
        import(&to.store, vault, to.media_path(), &archive)
    }

    #[test]
    fn the_history_moves_to_another_device_of_the_profile() {
        let vault = Vault::new([8; 32]);
        let (old, new) = (Device::new(), Device::new());
        for n in 1..=300 {
            put_message(&old, &vault, &text(u8::try_from(n % 250).unwrap()));
        }
        let status = vault.seal(
            Table::MessageStatus,
            &[1; 16],
            &MessageStatus::Read,
            &mut OsRng,
        );
        old.put(Table::MessageStatus, vec![1; 16], status);
        let (voice, copy) = put_note(&old, &vault, 7, 100_000);
        let (unfinished, _) = put_note(&old, &vault, 8, 10);
        let (gone, _) = put_note(&old, &vault, 9, 10);
        fs::remove_file(old.media_path()(&gone)).unwrap();

        hand_over(&old, &new, &vault, &[unfinished]).unwrap();
        assert_eq!(new.rows(Table::Messages), old.rows(Table::Messages));
        assert_eq!(
            new.rows(Table::MessageStatus),
            old.rows(Table::MessageStatus)
        );
        let media: Vec<Vec<u8>> = new.rows(Table::Media).into_iter().map(|(k, _)| k).collect();
        assert_eq!(media, vec![voice.to_vec()], "only whole notes move");
        assert_eq!(fs::read(new.media_path()(&voice)).unwrap(), copy);

        // Taken in again, nothing changes.
        hand_over(&old, &new, &vault, &[]).unwrap();
        assert_eq!(new.rows(Table::Messages), old.rows(Table::Messages));
    }

    #[test]
    fn what_the_new_device_has_already_stays() {
        let vault = Vault::new([8; 32]);
        let (old, new) = (Device::new(), Device::new());
        let m = text(1);
        let key = put_message(&old, &vault, &m);
        let mine = vault.seal(Table::Messages, &key, &m, &mut OsRng);
        new.put(Table::Messages, key.clone(), mine.clone());
        hand_over(&old, &new, &vault, &[]).unwrap();
        assert_eq!(new.rows(Table::Messages), vec![(key, mine)]);
    }

    #[test]
    fn rows_of_another_profile_or_a_broken_archive_are_refused() {
        let (vault, stranger) = (Vault::new([8; 32]), Vault::new([9; 32]));
        let (old, new) = (Device::new(), Device::new());
        put_message(&old, &stranger, &text(1));
        hand_over(&old, &new, &vault, &[]).unwrap();
        assert!(new.rows(Table::Messages).is_empty());

        let archive = old.media.with_file_name("broken.bin");
        fs::write(&archive, b"CYHX\x01").unwrap();
        assert!(import(&new.store, &vault, new.media_path(), &archive).is_err());
        assert!(import(&new.store, &vault, new.media_path(), &old.media).is_err());
    }
}
