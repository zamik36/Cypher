use std::collections::HashSet;

use cypher_common::Result;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::chunker::FileChunker;

/// Callback for sending a chunk over the transport layer.
///
/// Arguments: `(chunk_index, chunk_data, chunk_hash)`.
pub type ChunkSendFn = Box<
    dyn Fn(u32, Vec<u8>, Vec<u8>) -> futures::future::BoxFuture<'static, Result<()>> + Send + Sync,
>;

pub struct TransferSender {
    chunker: FileChunker,
    window_size: usize,
    acked: HashSet<u32>,
}

impl TransferSender {
    pub fn new(chunker: FileChunker, window_size: usize) -> Self {
        Self {
            chunker,
            window_size,
            acked: HashSet::new(),
        }
    }

    /// Run the transfer using window-based flow control.
    ///
    /// Sends up to `window_size` chunks ahead of the lowest un-acked index.
    /// When an ack is received the window slides forward.
    pub async fn run(
        &mut self,
        send_chunk: ChunkSendFn,
        mut ack_rx: mpsc::Receiver<u32>,
    ) -> Result<()> {
        let total = self.chunker.chunk_count();
        if total == 0 {
            return Ok(());
        }

        let mut next_to_send: u32 = 0;

        // Send the initial window.
        while next_to_send < total && (next_to_send as usize) < self.window_size {
            let (data, hash) = self.chunker.read_chunk(next_to_send).await?;
            debug!(chunk = next_to_send, "sending chunk");
            send_chunk(next_to_send, data, hash).await?;
            next_to_send += 1;
        }

        // Process acks and keep the window full.
        while self.acked.len() < total as usize {
            match ack_rx.recv().await {
                Some(ack_index) => {
                    debug!(chunk = ack_index, "received ack");
                    self.acked.insert(ack_index);

                    // Send more chunks to keep the window full.
                    // Window limit: next_to_send - (number of un-acked sent chunks) < window_size
                    while next_to_send < total {
                        let in_flight = next_to_send as usize - self.acked.len();
                        if in_flight >= self.window_size {
                            break;
                        }
                        let (data, hash) = self.chunker.read_chunk(next_to_send).await?;
                        debug!(chunk = next_to_send, "sending chunk");
                        send_chunk(next_to_send, data, hash).await?;
                        next_to_send += 1;
                    }
                }
                None => {
                    warn!("ack channel closed before transfer completed");
                    return Err(cypher_common::Error::Transfer(
                        "ack channel closed prematurely".into(),
                    ));
                }
            }
        }

        Ok(())
    }

    /// Run a selective transfer, sending only the specified chunk indices.
    ///
    /// Used for resume: the receiver reports which chunks are missing and we
    /// re-send only those.
    pub async fn run_selective(
        &mut self,
        indices: Vec<u32>,
        send_chunk: ChunkSendFn,
        mut ack_rx: mpsc::Receiver<u32>,
    ) -> Result<()> {
        let target_count = indices.len();
        if target_count == 0 {
            return Ok(());
        }

        let mut iter = indices.into_iter();
        let mut sent_count: usize = 0;

        // Send the initial window.
        while sent_count < self.window_size {
            let Some(idx) = iter.next() else { break };
            let (data, hash) = self.chunker.read_chunk(idx).await?;
            debug!(chunk = idx, "sending chunk (selective)");
            send_chunk(idx, data, hash).await?;
            sent_count += 1;
        }

        // Process acks and keep the window full.
        while self.acked.len() < target_count {
            match ack_rx.recv().await {
                Some(ack_index) => {
                    debug!(chunk = ack_index, "received ack (selective)");
                    self.acked.insert(ack_index);

                    // Send more chunks to keep the window full.
                    while sent_count - self.acked.len() < self.window_size {
                        let Some(idx) = iter.next() else { break };
                        let (data, hash) = self.chunker.read_chunk(idx).await?;
                        debug!(chunk = idx, "sending chunk (selective)");
                        send_chunk(idx, data, hash).await?;
                        sent_count += 1;
                    }
                }
                None => {
                    warn!("ack channel closed before selective transfer completed");
                    return Err(cypher_common::Error::Transfer(
                        "ack channel closed prematurely".into(),
                    ));
                }
            }
        }

        Ok(())
    }

    /// Progress as a fraction in `[0.0, 1.0]`.
    pub fn progress(&self) -> f64 {
        let total = self.chunker.chunk_count();
        if total == 0 {
            return 1.0;
        }
        self.acked.len() as f64 / total as f64
    }
}
