//! Positional file IO on a dedicated thread; jobs for one file run in order.

use std::collections::HashMap;
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::SystemTime;

use bytes::Bytes;
use cypher_core::Event;
use cypher_types::FileId;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// What a file we send looked like when it was offered.
///
/// Chunk nonces come from the chunk index, so reading a file that changed
/// since would encrypt different data under nonces already used; reads of a
/// source that no longer matches its stamp fail instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceStamp {
    len: u64,
    modified: SystemTime,
}

impl SourceStamp {
    pub(crate) fn of(meta: &Metadata) -> io::Result<Self> {
        Ok(Self {
            len: meta.len(),
            modified: meta.modified()?,
        })
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }
}

pub(crate) enum IoJob {
    Open {
        file_id: FileId,
        path: PathBuf,
        len: u64,
    },
    Write {
        file_id: FileId,
        offset: u64,
        data: Bytes,
    },
    Close {
        file_id: FileId,
        keep: bool,
    },
    Read(ChunkRead),
    /// Closes any handle to a temporary source and deletes it.
    Remove {
        file_id: FileId,
        path: PathBuf,
    },
    /// Hands `Event` back once every job before it is done.
    Notify(Event),
}

/// A chunk to read from a file we send, into a buffer with `headroom`
/// spare bytes in front.
pub(crate) struct ChunkRead {
    pub file_id: FileId,
    pub path: PathBuf,
    pub source: SourceStamp,
    pub index: u32,
    pub offset: u64,
    pub len: u32,
    pub headroom: usize,
}

pub(crate) enum IoDone {
    ChunkRead {
        file_id: FileId,
        index: u32,
        buf: Vec<u8>,
    },
    ReadFailed {
        file_id: FileId,
    },
    WriteFailed {
        file_id: FileId,
    },
    Notify(Event),
}

#[derive(Clone)]
pub(crate) struct FileIo {
    tx: std_mpsc::Sender<IoJob>,
}

impl FileIo {
    pub(crate) fn spawn(done: mpsc::UnboundedSender<IoDone>) -> io::Result<Self> {
        let (tx, rx) = std_mpsc::channel();
        thread::Builder::new()
            .name("cypher-files".into())
            .spawn(move || Worker::default().run(&rx, &done))?;
        Ok(Self { tx })
    }

    pub(crate) fn submit(&self, job: IoJob) {
        let _ = self.tx.send(job);
    }
}

#[derive(Default)]
struct Worker {
    sinks: HashMap<FileId, (File, PathBuf)>,
    sources: HashMap<FileId, File>,
}

impl Worker {
    fn run(mut self, rx: &std_mpsc::Receiver<IoJob>, done: &mpsc::UnboundedSender<IoDone>) {
        while let Ok(job) = rx.recv() {
            let event = match job {
                IoJob::Open { file_id, path, len } => self
                    .open(file_id, path, len)
                    .err()
                    .map(|_| IoDone::WriteFailed { file_id }),
                IoJob::Write {
                    file_id,
                    offset,
                    data,
                } => self
                    .write(file_id, offset, &data)
                    .err()
                    .map(|_| IoDone::WriteFailed { file_id }),
                IoJob::Close { file_id, keep } => {
                    self.close(file_id, keep);
                    None
                }
                IoJob::Read(job) => Some(match self.read(&job) {
                    Ok(buf) => IoDone::ChunkRead {
                        file_id: job.file_id,
                        index: job.index,
                        buf,
                    },
                    Err(_) => IoDone::ReadFailed {
                        file_id: job.file_id,
                    },
                }),
                IoJob::Remove { file_id, path } => {
                    self.sources.remove(&file_id);
                    let _ = std::fs::remove_file(path);
                    None
                }
                IoJob::Notify(event) => Some(IoDone::Notify(event)),
            };
            if let Some(event) = event {
                let _ = done.send(event);
            }
        }
    }

    fn open(&mut self, file_id: FileId, path: PathBuf, len: u64) -> io::Result<()> {
        if self.sinks.contains_key(&file_id) {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        if file.metadata()?.len() != len {
            file.set_len(len)?;
        }
        self.sinks.insert(file_id, (file, path));
        Ok(())
    }

    fn write(&self, file_id: FileId, offset: u64, data: &[u8]) -> io::Result<()> {
        let (file, _) = self.sinks.get(&file_id).ok_or(io::ErrorKind::NotFound)?;
        write_all_at(file, data, offset)
    }

    fn close(&mut self, file_id: FileId, keep: bool) {
        self.sources.remove(&file_id);
        if let Some((file, path)) = self.sinks.remove(&file_id) {
            if keep {
                let _ = file.sync_all();
            } else {
                drop(file);
                let _ = std::fs::remove_file(path);
            }
        }
    }

    fn read(&mut self, job: &ChunkRead) -> io::Result<Vec<u8>> {
        let file = match self.sources.entry(job.file_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(File::open(&job.path)?),
        };
        // One fstat per chunk, small next to reading and encrypting it.
        if SourceStamp::of(&file.metadata()?)? != job.source {
            return Err(io::Error::other("the file changed since it was offered"));
        }
        let mut buf = vec![0u8; job.headroom + job.len as usize];
        let (_, chunk) = buf.split_at_mut(job.headroom);
        read_exact_at(file, chunk, job.offset)?;
        Ok(buf)
    }
}

#[cfg(unix)]
pub(crate) fn read_exact_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, offset)
}

#[cfg(unix)]
fn write_all_at(f: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, buf, offset)
}

#[cfg(windows)]
pub(crate) fn read_exact_at(f: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match f.seek_read(buf, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = std::mem::take(&mut buf).split_at_mut(n).1;
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn write_all_at(f: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match f.seek_write(buf, offset)? {
            0 => return Err(io::ErrorKind::WriteZero.into()),
            n => {
                buf = buf.split_at(n).1;
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_land_at_their_offsets_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let (done_tx, mut done) = mpsc::unbounded_channel();
        let io = FileIo::spawn(done_tx).unwrap();
        let id = FileId([1; 16]);
        let path = dir.path().join("sub").join("f.bin");

        io.submit(IoJob::Open {
            file_id: id,
            path: path.clone(),
            len: 8,
        });
        for (offset, data) in [(4, b"WXYZ"), (0, b"abcd")] {
            io.submit(IoJob::Write {
                file_id: id,
                offset,
                data: Bytes::from_static(data),
            });
        }
        io.submit(IoJob::Close {
            file_id: id,
            keep: true,
        });
        // Jobs run in order: once a read answers, the writes are on disk.
        let unknown = SourceStamp {
            len: 0,
            modified: SystemTime::UNIX_EPOCH,
        };
        io.submit(read(id, &path, unknown, 0, 1));
        assert!(matches!(
            done.recv().await.unwrap(),
            IoDone::ReadFailed { .. }
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdWXYZ");

        let stamp = SourceStamp::of(&std::fs::metadata(&path).unwrap()).unwrap();
        io.submit(IoJob::Read(ChunkRead {
            headroom: 3,
            ..read_job(id, &path, stamp, 2, 4)
        }));
        match done.recv().await.unwrap() {
            IoDone::ChunkRead { buf, .. } => assert_eq!(&buf[3..], b"cdWX"),
            _ => panic!("expected read"),
        }
    }

    #[tokio::test]
    async fn a_discarded_sink_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let (done_tx, mut done) = mpsc::unbounded_channel();
        let io = FileIo::spawn(done_tx).unwrap();
        let id = FileId([2; 16]);
        let path = dir.path().join("partial.bin");
        io.submit(IoJob::Open {
            file_id: id,
            path: path.clone(),
            len: 4,
        });
        io.submit(IoJob::Close {
            file_id: id,
            keep: false,
        });
        let unknown = SourceStamp {
            len: 4,
            modified: SystemTime::UNIX_EPOCH,
        };
        io.submit(read(id, &path, unknown, 0, 1));
        assert!(matches!(
            done.recv().await.unwrap(),
            IoDone::ReadFailed { .. }
        ));
        assert!(!path.exists());
    }

    /// A source edited after it was offered, even to the same length, is not
    /// read again: its chunks would reuse nonces with different data.
    #[tokio::test]
    async fn a_source_changed_since_its_offer_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let (done_tx, mut done) = mpsc::unbounded_channel();
        let io = FileIo::spawn(done_tx).unwrap();
        let id = FileId([3; 16]);
        let path = dir.path().join("photo.jpg");
        std::fs::write(&path, b"original").unwrap();
        let stamp = SourceStamp::of(&std::fs::metadata(&path).unwrap()).unwrap();

        io.submit(read(id, &path, stamp, 0, 8));
        assert!(matches!(
            done.recv().await.unwrap(),
            IoDone::ChunkRead { .. }
        ));

        let edited = OpenOptions::new().write(true).open(&path).unwrap();
        write_all_at(&edited, b"replaced", 0).unwrap();
        // Set explicitly: a coarse clock may not tick between the writes.
        edited
            .set_modified(stamp.modified + std::time::Duration::from_secs(1))
            .unwrap();
        drop(edited);
        io.submit(read(id, &path, stamp, 0, 8));
        assert!(matches!(
            done.recv().await.unwrap(),
            IoDone::ReadFailed { .. }
        ));
    }

    fn read_job(
        file_id: FileId,
        path: &std::path::Path,
        source: SourceStamp,
        offset: u64,
        len: u32,
    ) -> ChunkRead {
        ChunkRead {
            file_id,
            path: path.to_owned(),
            source,
            index: 0,
            offset,
            len,
            headroom: 0,
        }
    }

    fn read(
        file_id: FileId,
        path: &std::path::Path,
        source: SourceStamp,
        offset: u64,
        len: u32,
    ) -> IoJob {
        IoJob::Read(read_job(file_id, path, source, offset, len))
    }
}
