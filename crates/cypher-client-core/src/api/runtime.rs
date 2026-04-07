use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, info, warn};

use cypher_common::{Error, FileId, FileMeta, PeerId, Result, DEFAULT_WINDOW_SIZE};
use cypher_nat::Candidate;
use cypher_proto::{dispatch, Message, Serializable};
use cypher_transfer::{ChunkSendFn, FileChunker, TransferReceiver, TransferSender};
use cypher_transport::{FrameFlags, TransportSession};

use crate::crypto::KeyManager;
use crate::onion;
use crate::persistence::MessageStore;

use super::{ClientEvent, OutboundCmd, PendingKind};

type PendingMap = DashMap<PendingKind, oneshot::Sender<serde_json::Value>>;
type PendingSends = DashMap<Vec<u8>, (Arc<Mutex<FileChunker>>, PeerId, FileMeta)>;
type ActiveReceives = DashMap<Vec<u8>, (Arc<Mutex<TransferReceiver>>, PeerId, bool)>;
type PendingMetas = DashMap<Vec<u8>, (FileMeta, PeerId)>;
type ActiveSends = DashMap<Vec<u8>, mpsc::Sender<u32>>;

#[derive(Clone)]
struct SendTaskContext {
    outbound_tx: mpsc::Sender<OutboundCmd>,
    event_tx: mpsc::Sender<ClientEvent>,
    keys: Arc<KeyManager>,
    active_sends: Arc<ActiveSends>,
}

#[derive(Clone)]
pub(super) struct RuntimeContext {
    pub(super) event_tx: mpsc::Sender<ClientEvent>,
    pub(super) pending: Arc<PendingMap>,
    pub(super) keys: Arc<KeyManager>,
    pub(super) outbound_tx: mpsc::Sender<OutboundCmd>,
    pub(super) pending_sends: Arc<PendingSends>,
    pub(super) active_recvs: Arc<ActiveReceives>,
    pub(super) pending_metas: Arc<PendingMetas>,
    pub(super) active_sends: Arc<ActiveSends>,
    pub(super) ice_agent: Arc<Mutex<Option<cypher_nat::IceAgent>>>,
    pub(super) message_store: Option<Arc<dyn MessageStore>>,
}

impl RuntimeContext {
    fn send_task_context(&self) -> SendTaskContext {
        SendTaskContext {
            outbound_tx: self.outbound_tx.clone(),
            event_tx: self.event_tx.clone(),
            keys: Arc::clone(&self.keys),
            active_sends: Arc::clone(&self.active_sends),
        }
    }

    pub(super) async fn dispatch_inbound(&self, payload: Bytes) {
        if payload.first() == Some(&b'{') {
            self.dispatch_json_payload(&payload).await;
            return;
        }

        match dispatch(&payload) {
            Ok(message) => self.handle_proto_message(message).await,
            Err(error) => debug!("unknown binary frame: {}", error),
        }
    }

    async fn dispatch_json_payload(&self, payload: &[u8]) {
        match serde_json::from_slice::<serde_json::Value>(payload) {
            Ok(json) => self.dispatch_json(json).await,
            Err(error) => warn!("malformed JSON from server: {}", error),
        }
    }

    async fn handle_proto_message(&self, message: Message) {
        match message {
            Message::ChatSend(chat) => self.handle_chat_message(chat).await,
            Message::FileOffer(offer) => self.handle_file_offer(offer).await,
            Message::FileAccept(accept) => self.handle_file_accept(accept, None).await,
            Message::FileChunk(chunk) => self.handle_file_chunk(chunk).await,
            Message::FileChunkAck(ack) => self.handle_file_chunk_ack(ack).await,
            Message::FileResume(resume) => {
                let missing = resume
                    .missing
                    .chunks_exact(4)
                    .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
                    .collect();
                self.handle_file_accept(
                    cypher_proto::FileAccept {
                        peer_id: resume.peer_id,
                        file_id: resume.file_id,
                    },
                    Some(missing),
                )
                .await;
            }
            Message::FileComplete(complete) => self.handle_file_complete(complete).await,
            Message::SignalIceCandidate(ice) => self.handle_ice_candidate(ice).await,
            Message::InboxMessages(inbox) => self.handle_inbox_messages(inbox).await,
            other => debug!("unhandled proto message: {:?}", other),
        }
    }

    async fn handle_chat_message(&self, chat: cypher_proto::ChatSend) {
        let Some(from) = PeerId::from_bytes(&chat.peer_id) else {
            warn!("ChatSend with invalid peer_id");
            return;
        };
        let rk_bytes: [u8; 32] = match chat.ratchet_key.as_slice().try_into() {
            Ok(bytes) => bytes,
            Err(_) => {
                warn!("ChatSend ratchet_key != 32 bytes");
                return;
            }
        };

        match self
            .keys
            .decrypt_from_peer(&chat.peer_id, &chat.ciphertext, &rk_bytes, chat.msg_no)
        {
            Ok(plaintext_raw) => {
                let plaintext = match onion::padding::unpad(&plaintext_raw) {
                    Ok(unpadded) => unpadded.to_vec(),
                    Err(_) => plaintext_raw,
                };
                self.persist_received_message(&from, &chat.peer_id, &plaintext);
                let _ = self
                    .event_tx
                    .send(ClientEvent::MessageReceived { from, plaintext })
                    .await;
            }
            Err(error) => {
                warn!("decryption failed: {}", error);
                self.emit_error(format!("decrypt: {error}")).await;
            }
        }
    }

    async fn handle_file_offer(&self, offer: cypher_proto::FileOffer) {
        let Some(from) = PeerId::from_bytes(&offer.peer_id) else {
            warn!("FileOffer with invalid sender peer_id");
            return;
        };
        let meta = FileMeta {
            file_id: FileId::from_bytes(&offer.file_id).unwrap_or_else(FileId::generate),
            name: offer.name,
            size: offer.size,
            chunk_count: offer.chunks,
            hash: Bytes::from(offer.hash),
            compressed: offer.compressed != 0,
        };
        self.pending_metas
            .insert(offer.file_id, (meta.clone(), from.clone()));
        let _ = self
            .event_tx
            .send(ClientEvent::FileOffered { from, meta })
            .await;
    }

    async fn handle_file_accept(
        &self,
        accept: cypher_proto::FileAccept,
        selective_indices: Option<Vec<u32>>,
    ) {
        let Some(entry) = self.pending_sends.remove(&accept.file_id) else {
            warn!(
                file_id = ?accept.file_id,
                "file send continuation for unknown file_id"
            );
            return;
        };
        let (chunker_arc, peer_id, meta) = entry.1;
        let file_id = accept.file_id;

        let (ack_tx, ack_rx) = mpsc::channel::<u32>(DEFAULT_WINDOW_SIZE * 2);
        self.active_sends.insert(file_id.clone(), ack_tx);

        let send_context = self.send_task_context();
        tokio::spawn(async move {
            send_chunks(
                chunker_arc,
                file_id,
                peer_id,
                meta,
                ack_rx,
                selective_indices,
                send_context,
            )
            .await;
        });
    }

    async fn handle_file_chunk(&self, chunk: cypher_proto::FileChunk) {
        let receive_state = self.active_recvs.get(&chunk.file_id).map(|entry| {
            let (receiver, peer_id, compressed) = entry.value();
            (Arc::clone(receiver), peer_id.clone(), *compressed)
        });
        let Some((receiver, sender_peer_id, is_compressed)) = receive_state else {
            warn!("FileChunk for unknown file_id");
            return;
        };

        let decrypted = match self.keys.decrypt_from_peer(
            sender_peer_id.as_bytes(),
            &chunk.data,
            &chunk.ratchet_key,
            chunk.msg_no,
        ) {
            Ok(plaintext) => plaintext,
            Err(error) => {
                warn!("chunk decrypt failed: {}", error);
                self.emit_error(format!("chunk decrypt: {error}")).await;
                return;
            }
        };

        let plaintext = if is_compressed {
            match cypher_transfer::decompress_chunk(&decrypted) {
                Ok(decompressed) => decompressed,
                Err(error) => {
                    warn!("chunk decompress failed: {}", error);
                    self.emit_error(format!("chunk decompress: {error}")).await;
                    return;
                }
            }
        } else {
            decrypted
        };

        let done = {
            let mut recv = receiver.lock().await;
            match recv
                .handle_chunk(chunk.index, &plaintext, &chunk.hash)
                .await
            {
                Ok(done) => done,
                Err(error) => {
                    warn!("chunk write failed: {}", error);
                    self.emit_error(format!("chunk error: {error}")).await;
                    return;
                }
            }
        };

        self.send_chunk_ack(&sender_peer_id, &chunk.file_id, chunk.index)
            .await;

        let progress = receiver.lock().await.progress();
        let _ = self
            .event_tx
            .send(ClientEvent::FileProgress {
                file_id: chunk.file_id.clone(),
                progress,
            })
            .await;

        if done {
            self.finalize_received_file(receiver, chunk.file_id).await;
        }
    }

    async fn send_chunk_ack(&self, peer_id: &PeerId, file_id: &[u8], index: u32) {
        let ack = cypher_proto::FileChunkAck {
            peer_id: peer_id.to_vec(),
            file_id: file_id.to_vec(),
            index,
        };
        let _ = self
            .outbound_tx
            .send(OutboundCmd::Send {
                payload: Bytes::from(ack.serialize()),
                flags: FrameFlags::NONE,
            })
            .await;
    }

    async fn finalize_received_file(
        &self,
        receiver: Arc<Mutex<TransferReceiver>>,
        file_id: Vec<u8>,
    ) {
        let (verified, receiver_clone) = {
            let recv = receiver.lock().await;
            (recv.verify().await, receiver.clone())
        };
        self.active_recvs.remove(&file_id);

        match verified {
            Ok(true) => {
                receiver_clone.lock().await.cleanup_state().await;
                let _ = self
                    .event_tx
                    .send(ClientEvent::FileComplete { file_id })
                    .await;
            }
            Ok(false) => {
                self.emit_error("file integrity verification failed".to_string())
                    .await;
            }
            Err(error) => {
                self.emit_error(format!("file verification error: {error}"))
                    .await;
            }
        }
    }

    async fn handle_file_chunk_ack(&self, ack: cypher_proto::FileChunkAck) {
        let sender = self
            .active_sends
            .get(&ack.file_id)
            .map(|entry| entry.value().clone());
        if let Some(sender) = sender {
            let _ = sender.send(ack.index).await;
        }
    }

    async fn handle_file_complete(&self, complete: cypher_proto::FileComplete) {
        self.active_recvs.remove(&complete.file_id);
        let _ = self
            .event_tx
            .send(ClientEvent::FileComplete {
                file_id: complete.file_id,
            })
            .await;
    }

    async fn handle_ice_candidate(&self, ice: cypher_proto::SignalIceCandidate) {
        match ice.candidate.parse::<SocketAddr>() {
            Ok(addr) => {
                let candidate = Candidate::server_reflexive(addr);
                debug!(addr = %addr, "received remote ICE candidate");
                if let Some(agent) = self.ice_agent.lock().await.as_mut() {
                    agent.add_remote_candidate(candidate.clone());
                }
                let _ = self
                    .event_tx
                    .send(ClientEvent::IceCandidateReceived {
                        from: ice.peer_id,
                        candidate,
                    })
                    .await;
            }
            Err(error) => {
                warn!(candidate = %ice.candidate, error = %error, "invalid ICE candidate address");
            }
        }
    }

    async fn handle_inbox_messages(&self, inbox: cypher_proto::InboxMessages) {
        let mut offset = 0usize;
        let mut delivered = 0u32;

        while offset + 4 <= inbox.messages.len() {
            let len =
                u32::from_le_bytes(inbox.messages[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            if offset + len > inbox.messages.len() {
                break;
            }

            let message_bytes = &inbox.messages[offset..offset + len];
            offset += len;

            if self.handle_inbox_message(message_bytes).await {
                delivered += 1;
            }
        }

        if delivered > 0 {
            info!(delivered, "processed inbox messages");
        }
    }

    async fn handle_inbox_message(&self, message_bytes: &[u8]) -> bool {
        match dispatch(message_bytes) {
            Ok(Message::ChatSend(chat)) => self.dispatch_inbox_chat(chat).await,
            Ok(_) => {
                debug!("inbox contained non-ChatSend message, skipping");
                false
            }
            Err(error) => {
                debug!("malformed inbox message: {}", error);
                false
            }
        }
    }

    async fn dispatch_inbox_chat(&self, chat: cypher_proto::ChatSend) -> bool {
        let Some(from) = PeerId::from_bytes(&chat.peer_id) else {
            return false;
        };
        let rk_bytes: [u8; 32] = match chat.ratchet_key.as_slice().try_into() {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };

        match self
            .keys
            .decrypt_from_peer(&chat.peer_id, &chat.ciphertext, &rk_bytes, chat.msg_no)
        {
            Ok(plaintext_raw) => {
                let plaintext = match onion::padding::unpad(&plaintext_raw) {
                    Ok(unpadded) => unpadded.to_vec(),
                    Err(_) => plaintext_raw,
                };
                self.persist_received_message(&from, &chat.peer_id, &plaintext);
                let _ = self
                    .event_tx
                    .send(ClientEvent::MessageReceived { from, plaintext })
                    .await;
                true
            }
            Err(error) => {
                warn!("inbox message decrypt failed: {error}");
                false
            }
        }
    }

    fn persist_received_message(&self, from: &PeerId, peer_key: &[u8], plaintext: &[u8]) {
        let Some(store) = &self.message_store else {
            return;
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Err(error) = store.save_message(
            from,
            crate::persistence::Direction::Received,
            plaintext,
            now,
        ) {
            warn!("failed to persist received message: {error}");
        }
        if let Some(state) = self.keys.get_ratchet_state(peer_key) {
            if let Err(error) = store.save_ratchet_state(from, &state) {
                warn!("failed to persist ratchet state: {error}");
            }
        }
    }

    async fn emit_error(&self, message: String) {
        let _ = self.event_tx.send(ClientEvent::Error(message)).await;
    }

    async fn dispatch_json(&self, json: serde_json::Value) {
        if json.get("peer_joined").and_then(|value| value.as_bool()) == Some(true) {
            if let Some(hex) = json.get("peer_id").and_then(|value| value.as_str()) {
                if let Ok(bytes) = hex_decode(hex) {
                    if let Some(peer_id) = PeerId::from_bytes(&bytes) {
                        let _ = self
                            .event_tx
                            .send(ClientEvent::PeerConnected { peer_id })
                            .await;
                    }
                }
            }
            return;
        }

        let kind = if json.get("link_id").is_some() {
            PendingKind::CreateLink
        } else if json.get("peer_id").is_some() {
            PendingKind::JoinLink
        } else if json.get("identity_key").is_some() || json.get("signed_prekey").is_some() {
            PendingKind::GetPrekeys
        } else {
            debug!("unrecognised signaling JSON: {:?}", json);
            return;
        };

        if let Some((_, tx)) = self.pending.remove(&kind) {
            let _ = tx.send(json);
        }
    }
}

pub(super) async fn run_io_loop(
    mut session: TransportSession,
    mut outbound_rx: mpsc::Receiver<OutboundCmd>,
    context: RuntimeContext,
) {
    loop {
        tokio::select! {
            cmd = outbound_rx.recv() => match cmd {
                Some(OutboundCmd::Send { payload, flags }) => {
                    if let Err(error) = session.send_frame(payload, flags).await {
                        warn!("gateway write error: {}", error);
                        break;
                    }
                }
                Some(OutboundCmd::Close) | None => {
                    let _ = session.close().await;
                    break;
                }
            },
            result = session.recv_frame() => match result {
                Ok(frame) => context.dispatch_inbound(frame.payload).await,
                Err(cypher_common::Error::ConnectionClosed) => {
                    info!("gateway connection closed");
                    break;
                }
                Err(error) => {
                    warn!("gateway read error: {}", error);
                    break;
                }
            },
        }
    }

    let _ = context.event_tx.send(ClientEvent::Disconnected).await;
}

async fn send_chunks(
    chunker_mu: Arc<Mutex<FileChunker>>,
    file_id: Vec<u8>,
    peer_id: PeerId,
    meta: FileMeta,
    ack_rx: mpsc::Receiver<u32>,
    selective_indices: Option<Vec<u32>>,
    context: SendTaskContext,
) {
    let chunker = match Arc::try_unwrap(chunker_mu) {
        Ok(mutex) => mutex.into_inner(),
        Err(_) => {
            let _ = context
                .event_tx
                .send(ClientEvent::Error(
                    "file send failed: chunker still referenced".into(),
                ))
                .await;
            return;
        }
    };

    let mut sender = TransferSender::new(chunker, DEFAULT_WINDOW_SIZE);
    let outbound_tx = context.outbound_tx.clone();
    let event_tx = context.event_tx.clone();
    let keys = Arc::clone(&context.keys);
    let active_sends = Arc::clone(&context.active_sends);
    let compressed = meta.compressed;
    let file_id_for_chunks = file_id.clone();
    let peer_id_for_chunks = peer_id.clone();

    let send_fn: ChunkSendFn = Box::new(move |index, data, hash| {
        let keys = Arc::clone(&keys);
        let peer_id = peer_id_for_chunks.clone();
        let file_id = file_id_for_chunks.clone();
        let outbound_tx = outbound_tx.clone();
        let event_tx = event_tx.clone();
        Box::pin(async move {
            let send_data = if compressed {
                cypher_transfer::compress_chunk(&data)?
            } else {
                data.to_vec()
            };
            let (ciphertext, ratchet_key, msg_no) =
                keys.encrypt_for_peer(peer_id.as_bytes(), &send_data)?;
            let chunk = cypher_proto::FileChunk {
                peer_id: peer_id.to_vec(),
                file_id: file_id.clone(),
                index,
                data: ciphertext,
                hash,
                ratchet_key,
                msg_no,
            };
            outbound_tx
                .send(OutboundCmd::Send {
                    payload: Bytes::from(chunk.serialize()),
                    flags: FrameFlags::NONE,
                })
                .await
                .map_err(|_| Error::Session("outbound closed".into()))?;
            let _ = event_tx
                .send(ClientEvent::FileProgress {
                    file_id,
                    progress: -1.0,
                })
                .await;
            Ok(())
        })
    });

    let result = match selective_indices {
        Some(indices) => sender.run_selective(indices, send_fn, ack_rx).await,
        None => sender.run(send_fn, ack_rx).await,
    };

    active_sends.remove(&file_id);

    if let Err(error) = result {
        warn!("windowed transfer failed: {}", error);
        let _ = context
            .event_tx
            .send(ClientEvent::Error(format!("transfer: {error}")))
            .await;
        return;
    }

    let complete = cypher_proto::FileComplete {
        peer_id: peer_id.to_vec(),
        file_id: file_id.clone(),
    };
    let _ = context
        .outbound_tx
        .send(OutboundCmd::Send {
            payload: Bytes::from(complete.serialize()),
            flags: FrameFlags::NONE,
        })
        .await;

    info!(file = %meta.name, "file transfer complete");
    let _ = context
        .event_tx
        .send(ClientEvent::FileComplete { file_id })
        .await;
}

pub(super) fn json_bytes_field(obj: &serde_json::Value, field: &str) -> Result<Vec<u8>> {
    let arr = obj
        .get(field)
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::Protocol(format!("missing or non-array field '{field}'")))?;
    arr.iter()
        .map(|v| {
            v.as_u64()
                .and_then(|n| u8::try_from(n).ok())
                .ok_or_else(|| Error::Protocol(format!("invalid byte in '{field}'")))
        })
        .collect()
}

pub(super) fn hex_decode(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return Err(Error::Protocol("odd-length hex string".into()));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| Error::Protocol(format!("invalid hex at offset {i}")))
        })
        .collect()
}
