use cypher_common::Result;

use crate::assembler::FileAssembler;

pub struct TransferReceiver {
    assembler: FileAssembler,
}

impl TransferReceiver {
    pub fn new(assembler: FileAssembler) -> Self {
        Self { assembler }
    }

    /// Handle an incoming chunk. Returns `true` when the transfer is complete.
    pub async fn handle_chunk(&mut self, index: u32, data: &[u8], hash: &[u8]) -> Result<bool> {
        self.assembler.write_chunk(index, data, hash).await?;
        Ok(self.assembler.is_complete())
    }

    /// Progress as a fraction in `[0.0, 1.0]`.
    pub fn progress(&self) -> f64 {
        self.assembler.progress()
    }

    /// Returns the list of chunk indices not yet received.
    pub fn missing_chunks(&self) -> Vec<u32> {
        self.assembler.missing_chunks()
    }

    /// Verify the full file hash after assembly is complete.
    pub async fn verify(&self) -> Result<bool> {
        self.assembler.verify().await
    }

    /// Remove the sidecar state file after a successful transfer.
    pub async fn cleanup_state(&self) {
        self.assembler.cleanup_state().await;
    }
}
