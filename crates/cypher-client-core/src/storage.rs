use dashmap::DashMap;
use cypher_common::{FileId, FileMeta};
use std::path::PathBuf;

/// Ephemeral in-memory store that tracks in-progress file transfers.
///
/// No data is written to disk by this type itself; it only records the
/// source/destination paths and metadata while a transfer is active.
pub struct LocalStorage {
    /// file_id bytes -> source path (outgoing transfers)
    pending_sends: DashMap<Vec<u8>, PathBuf>,
    /// file_id bytes -> (meta, destination path) (incoming transfers)
    pending_recvs: DashMap<Vec<u8>, (FileMeta, PathBuf)>,
}

impl LocalStorage {
    pub fn new() -> Self {
        Self {
            pending_sends: DashMap::new(),
            pending_recvs: DashMap::new(),
        }
    }

    /// Register an outgoing file transfer.
    pub fn add_pending_send(&self, file_id: &FileId, path: PathBuf) {
        self.pending_sends.insert(file_id.to_vec(), path);
    }

    /// Register an incoming file transfer.
    pub fn add_pending_recv(&self, file_id: &FileId, meta: FileMeta, dest: PathBuf) {
        self.pending_recvs.insert(file_id.to_vec(), (meta, dest));
    }

    /// Look up an incoming transfer by its raw file-id bytes.
    pub fn get_pending_recv(&self, file_id: &[u8]) -> Option<(FileMeta, PathBuf)> {
        self.pending_recvs
            .get(file_id)
            .map(|entry| entry.value().clone())
    }

    /// Remove a completed or cancelled transfer (either direction).
    pub fn remove_transfer(&self, file_id: &[u8]) {
        self.pending_sends.remove(file_id);
        self.pending_recvs.remove(file_id);
    }
}

impl Default for LocalStorage {
    fn default() -> Self {
        Self::new()
    }
}
