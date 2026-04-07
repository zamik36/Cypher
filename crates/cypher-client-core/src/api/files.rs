use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::Mutex;
use tracing::info;

use cypher_common::{Error, FileMeta, Result};
use cypher_proto::Serializable;
use cypher_transfer::{FileAssembler, TransferReceiver};
use cypher_transport::FrameFlags;

use crate::transfer::TransferManager;

use super::ClientApi;

impl ClientApi {
    /// Offer a file to `peer_id`.
    pub async fn send_file(
        &self,
        peer_id: &cypher_common::PeerId,
        path: &Path,
    ) -> Result<FileMeta> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".to_string());

        let (meta, chunker) = TransferManager::prepare_send(path, name).await?;
        let file_id = meta.file_id.to_vec();

        let offer = cypher_proto::FileOffer {
            peer_id: peer_id.to_vec(),
            file_id: file_id.clone(),
            name: meta.name.clone(),
            size: meta.size,
            chunks: meta.chunk_count,
            hash: meta.hash.to_vec(),
            compressed: if meta.compressed { 1 } else { 0 },
        };
        self.send_raw(Bytes::from(offer.serialize()), FrameFlags::NONE)
            .await?;

        self.pending_sends.insert(
            file_id,
            (Arc::new(Mutex::new(chunker)), peer_id.clone(), meta.clone()),
        );
        info!(peer_id = %peer_id, file = %meta.name, "FileOffer sent");
        Ok(meta)
    }

    /// Accept an incoming file offer and start receiving chunks.
    pub async fn accept_file(&self, file_id: &[u8], dest_path: &Path) -> Result<()> {
        let (meta, sender_peer_id) = self
            .pending_metas
            .remove(file_id)
            .map(|(_, v)| v)
            .ok_or_else(|| Error::Protocol("no pending file offer for that id".into()))?;

        let (assembler, missing) = match FileAssembler::load_state(dest_path, &meta).await? {
            Some(asm) => {
                let missing = asm.missing_chunks();
                (asm, Some(missing))
            }
            None => (FileAssembler::new(dest_path, &meta).await?, None),
        };

        let receiver = TransferReceiver::new(assembler);

        let is_compressed = meta.compressed;
        self.active_recvs.insert(
            file_id.to_vec(),
            (
                Arc::new(Mutex::new(receiver)),
                sender_peer_id.clone(),
                is_compressed,
            ),
        );

        if let Some(missing) = missing {
            let packed: Vec<u8> = missing
                .iter()
                .flat_map(|index| index.to_le_bytes())
                .collect();
            let msg = cypher_proto::FileResume {
                peer_id: sender_peer_id.to_vec(),
                file_id: file_id.to_vec(),
                missing: packed,
            };
            self.send_raw(Bytes::from(msg.serialize()), FrameFlags::NONE)
                .await?;
            info!(file_id = ?file_id, "FileResume sent (resume)");
        } else {
            let accept = cypher_proto::FileAccept {
                peer_id: sender_peer_id.to_vec(),
                file_id: file_id.to_vec(),
            };
            self.send_raw(Bytes::from(accept.serialize()), FrameFlags::NONE)
                .await?;
            info!(file_id = ?file_id, "FileAccept sent");
        }
        Ok(())
    }
}
