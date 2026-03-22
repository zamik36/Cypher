use cypher_common::FileId;

#[derive(Debug, Clone)]
pub struct TransferProgress {
    pub file_id: FileId,
    pub file_name: String,
    pub total_size: u64,
    pub transferred: u64,
    pub chunks_done: u32,
    pub chunks_total: u32,
    pub speed_bps: u64,
}
