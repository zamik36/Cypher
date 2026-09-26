//! Positional file IO on a dedicated thread; jobs for one file run in order.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::thread;

use bytes::Bytes;
use cypher_types::FileId;
use tokio::sync::mpsc;

pub enum IoJob {
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
    Read {
        file_id: FileId,
        path: PathBuf,
        index: u32,
        offset: u64,
        len: u32,
        headroom: usize,
    },
}

pub enum IoDone {
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
}

#[derive(Clone)]
pub struct FileIo {
    tx: std_mpsc::Sender<IoJob>,
}

impl FileIo {
    pub fn spawn(done: mpsc::UnboundedSender<IoDone>) -> io::Result<Self> {
        let (tx, rx) = std_mpsc::channel();
        thread::Builder::new()
            .name("cypher-files".into())
            .spawn(move || Worker::default().run(&rx, &done))?;
        Ok(Self { tx })
    }

    pub fn submit(&self, job: IoJob) {
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
                IoJob::Read {
                    file_id,
                    path,
                    index,
                    offset,
                    len,
                    headroom,
                } => Some(match self.read(file_id, &path, offset, len, headroom) {
                    Ok(buf) => IoDone::ChunkRead {
                        file_id,
                        index,
                        buf,
                    },
                    Err(_) => IoDone::ReadFailed { file_id },
                }),
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

    fn read(
        &mut self,
        file_id: FileId,
        path: &PathBuf,
        offset: u64,
        len: u32,
        headroom: usize,
    ) -> io::Result<Vec<u8>> {
        let file = match self.sources.entry(file_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(File::open(path)?),
        };
        let mut buf = vec![0u8; headroom + len as usize];
        read_exact_at(file, &mut buf[headroom..], offset)?;
        Ok(buf)
    }
}

#[cfg(unix)]
fn read_exact_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, offset)
}

#[cfg(unix)]
fn write_all_at(f: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, buf, offset)
}

#[cfg(windows)]
fn read_exact_at(f: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match f.seek_read(buf, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
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
                buf = &buf[n..];
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
    async fn write_read_close_and_discard() {
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
        io.submit(IoJob::Write {
            file_id: id,
            offset: 4,
            data: Bytes::from_static(b"WXYZ"),
        });
        io.submit(IoJob::Write {
            file_id: id,
            offset: 0,
            data: Bytes::from_static(b"abcd"),
        });
        io.submit(IoJob::Close {
            file_id: id,
            keep: true,
        });
        io.submit(IoJob::Read {
            file_id: id,
            path: path.clone(),
            index: 0,
            offset: 2,
            len: 4,
            headroom: 3,
        });
        match done.recv().await.unwrap() {
            IoDone::ChunkRead { buf, .. } => assert_eq!(&buf[3..], b"cdWX"),
            _ => panic!("expected read"),
        }

        let tmp = FileId([2; 16]);
        let tmp_path = dir.path().join("partial.bin");
        io.submit(IoJob::Open {
            file_id: tmp,
            path: tmp_path.clone(),
            len: 4,
        });
        io.submit(IoJob::Close {
            file_id: tmp,
            keep: false,
        });
        io.submit(IoJob::Read {
            file_id: tmp,
            path: tmp_path.clone(),
            index: 0,
            offset: 0,
            len: 1,
            headroom: 0,
        });
        assert!(matches!(
            done.recv().await.unwrap(),
            IoDone::ReadFailed { .. }
        ));
        assert!(!tmp_path.exists());
    }
}
