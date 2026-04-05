//! High-level P2P client API for UI layers (Tauri desktop, UniFFI mobile).
//!
//! # Architecture
//!
//! `ClientApi` owns the local ephemeral identity and key-management state.
//! When [`connect_to_gateway`](ClientApi::connect_to_gateway) is called:
//!
//! 1. A TLS connection is opened via [`TransportSession`].
//! 2. `SESSION_INIT` is exchanged; the gateway registers the peer.
//! 3. Our X3DH prekeys are uploaded to the signaling service.
//! 4. A background task is spawned that simultaneously reads incoming frames
//!    and writes outbound frames, emitting [`ClientEvent`]s to the channel
//!    consumed by [`next_event`](ClientApi::next_event).

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use dashmap::DashMap;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, info, warn};
use x25519_dalek::PublicKey as X25519PublicKey;

use cypher_common::{Error, FileId, FileMeta, PeerId, Result, DEFAULT_WINDOW_SIZE};
use cypher_nat::{Candidate, IceAgent};
use cypher_proto::{dispatch, Message, Serializable};
use cypher_transfer::{ChunkSendFn, FileAssembler, FileChunker, TransferReceiver, TransferSender};
use cypher_transport::{FrameFlags, TransportSession};

use crate::connection::ServerConnection;
use crate::crypto::KeyManager;
use crate::persistence::MessageStore;
use crate::session::ClientSession;
use crate::signaling::SignalingClient;
use crate::transfer::TransferManager;

/// An event emitted by the P2P subsystem to the UI layer.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// Successfully registered with the gateway.
    Connected { peer_id: PeerId },
    /// Connection to the gateway lost.
    Disconnected,
    /// A decrypted message from a remote peer.
    MessageReceived { from: PeerId, plaintext: Vec<u8> },
    /// A remote peer is offering to send a file.
    FileOffered { from: PeerId, meta: FileMeta },
    /// Progress update for an in-flight file transfer (`[0.0, 1.0]`).
    FileProgress { file_id: Vec<u8>, progress: f64 },
    /// A file transfer completed successfully.
    FileComplete { file_id: Vec<u8> },
    /// A remote peer has joined our link and is ready for session setup.
    PeerConnected { peer_id: PeerId },
    /// A remote peer sent an ICE candidate for NAT traversal.
    IceCandidateReceived { from: Vec<u8>, candidate: Candidate },
    /// A non-fatal error the UI may want to display.
    Error(String),
}

enum OutboundCmd {
    Send {
        payload: Bytes,
        flags: FrameFlags,
    },
    #[allow(dead_code)]
    Close,
}

/// Discriminates which signaling request an asynchronous JSON response belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PendingKind {
    CreateLink,
    JoinLink,
    GetPrekeys,
}

#[allow(clippy::type_complexity)]
pub struct ClientApi {
    session: Arc<ClientSession>,
    keys: Arc<KeyManager>,
    outbound_tx: Mutex<Option<mpsc::Sender<OutboundCmd>>>,
    pending: Arc<DashMap<PendingKind, oneshot::Sender<serde_json::Value>>>,
    event_tx: mpsc::Sender<ClientEvent>,
    event_rx: Mutex<mpsc::Receiver<ClientEvent>>,
    /// file_id → (FileChunker, sender_peer_id, FileMeta) waiting for FileAccept.
    pending_sends: Arc<DashMap<Vec<u8>, (Arc<Mutex<FileChunker>>, PeerId, FileMeta)>>,
    /// file_id → (TransferReceiver, sender_peer_id, compressed) for in-flight receives.
    active_recvs: Arc<DashMap<Vec<u8>, (Arc<Mutex<TransferReceiver>>, PeerId, bool)>>,
    /// file_id → (FileMeta, sender_peer_id) for offered files awaiting accept_file().
    pending_metas: Arc<DashMap<Vec<u8>, (FileMeta, PeerId)>>,
    /// file_id → ack channel sender for windowed TransferSender tasks.
    active_sends: Arc<DashMap<Vec<u8>, mpsc::Sender<u32>>>,
    /// ICE agent for NAT traversal (set during gather_candidates).
    ice_agent: Arc<Mutex<Option<IceAgent>>>,
    /// Direct P2P UDP socket after successful hole punch.
    p2p_socket: Mutex<Option<Arc<UdpSocket>>>,
    /// Relay client for TURN-like fallback (set during connect_relay).
    relay_client: Arc<Mutex<Option<cypher_nat::RelayClient>>>,
    /// Optional persistent message store for chat history.
    message_store: Option<Arc<dyn MessageStore>>,
    /// Blind inbox ID derived from identity seed (for offline message delivery).
    inbox_id: Vec<u8>,
}

impl ClientApi {
    /// Create a new client with an ephemeral (random) identity.
    pub fn new() -> Self {
        let session = Arc::new(ClientSession::new());
        let keys = Arc::new(KeyManager::new(
            cypher_crypto::identity::IdentityKeyPair::generate(),
        ));
        Self::build(session, keys, None, Vec::new())
    }

    /// Create a new client with a persistent identity derived from a seed,
    /// and an optional message store for chat history.
    pub fn with_seed(
        seed: &cypher_crypto::IdentitySeed,
        message_store: Option<Arc<dyn MessageStore>>,
    ) -> Self {
        // Derive two independent copies of the keypair (deterministic from seed).
        let session_identity = seed.derive_identity();
        let keys_identity = seed.derive_identity();
        let inbox_id = seed.derive_inbox_id().to_vec();
        let session = Arc::new(ClientSession::from_identity(session_identity));
        let keys = Arc::new(KeyManager::new(keys_identity));
        Self::build(session, keys, message_store, inbox_id)
    }

    fn build(
        session: Arc<ClientSession>,
        keys: Arc<KeyManager>,
        message_store: Option<Arc<dyn MessageStore>>,
        inbox_id: Vec<u8>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);
        Self {
            session,
            keys,
            outbound_tx: Mutex::new(None),
            pending: Arc::new(DashMap::new()),
            event_tx,
            event_rx: Mutex::new(event_rx),
            pending_sends: Arc::new(DashMap::new()),
            active_recvs: Arc::new(DashMap::new()),
            pending_metas: Arc::new(DashMap::new()),
            active_sends: Arc::new(DashMap::new()),
            ice_agent: Arc::new(Mutex::new(None)),
            p2p_socket: Mutex::new(None),
            relay_client: Arc::new(Mutex::new(None)),
            message_store,
            inbox_id,
        }
    }

    /// Return the blind inbox ID (empty for ephemeral identities).
    pub fn inbox_id(&self) -> &[u8] {
        &self.inbox_id
    }

    pub fn peer_id(&self) -> &PeerId {
        self.session.peer_id()
    }

    /// Access the key manager (for restoring ratchet states, etc.).
    pub fn keys(&self) -> &Arc<KeyManager> {
        &self.keys
    }

    /// Connect to the gateway over TLS.
    ///
    /// Uses an insecure verifier that accepts self-signed certificates so that
    /// development setups (where the gateway generates a fresh self-signed cert
    /// on every start) work out of the box.  In production, switch to
    /// [`connect_to_gateway_with_config`] with a proper [`rustls::ClientConfig`].
    pub async fn connect_to_gateway(&self, addr: &str) -> Result<()> {
        #[cfg(debug_assertions)]
        let tls_config = cypher_tls::make_client_config_insecure();
        #[cfg(not(debug_assertions))]
        let tls_config = cypher_tls::make_client_config();
        self.do_connect(addr, tls_config).await
    }

    /// Connect to the gateway with a specific [`rustls::ClientConfig`] (e.g.
    /// for testing with self-signed certificates).
    pub async fn connect_to_gateway_with_config(
        &self,
        addr: &str,
        tls_config: Arc<rustls::ClientConfig>,
    ) -> Result<()> {
        self.do_connect(addr, tls_config).await
    }

    async fn do_connect(&self, addr: &str, tls_config: Arc<rustls::ClientConfig>) -> Result<()> {
        info!(addr, "do_connect: starting TLS connection...");
        let conn = ServerConnection::connect_tls(addr, tls_config).await?;
        info!(addr, peer_id = %self.session.peer_id(), "do_connect: TLS established");

        // SESSION_INIT handshake (with timeout).
        let mut signaling = SignalingClient::new(conn);
        let nonce: [u8; 32] = rand::random();
        info!("do_connect: sending SESSION_INIT...");
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            signaling.session_init(self.session.peer_id().to_vec(), nonce.to_vec()),
        )
        .await
        {
            Ok(Ok(_)) => info!("do_connect: SESSION_INIT completed"),
            Ok(Err(e)) => {
                warn!("do_connect: SESSION_INIT failed: {e}");
                return Err(e);
            }
            Err(_) => {
                warn!("do_connect: SESSION_INIT timed out after 10s");
                return Err(Error::Transport("SESSION_INIT timed out".into()));
            }
        }

        // Upload prekeys (fire-and-forget).
        let bundle = self.keys.key_bundle();
        let raw = bundle.to_bytes();
        info!("do_connect: uploading prekeys...");
        signaling
            .upload_prekeys(
                raw[32..64].to_vec(), // identity_dh_key
                raw[64..96].to_vec(), // signed_prekey
                self.inbox_id.clone(),
            )
            .await?;
        info!("do_connect: prekeys uploaded");

        // Hand the underlying TransportSession to the background I/O task.
        let (outbound_tx, outbound_rx) = mpsc::channel::<OutboundCmd>(256);
        *self.outbound_tx.lock().await = Some(outbound_tx.clone());
        self.spawn_io_task(signaling.conn.into_session(), outbound_rx, outbound_tx);

        info!("do_connect: sending Connected event");
        let _ = self
            .event_tx
            .send(ClientEvent::Connected {
                peer_id: self.session.peer_id().clone(),
            })
            .await;

        // Fetch any queued offline messages from our blind inbox.
        if !self.inbox_id.is_empty() {
            let inbox_msg = cypher_proto::InboxFetch {
                inbox_id: self.inbox_id.clone(),
            };
            if let Some(tx) = self.outbound_tx.lock().await.as_ref() {
                let _ = tx
                    .send(OutboundCmd::Send {
                        payload: Bytes::from(inbox_msg.serialize()),
                        flags: FrameFlags::NONE,
                    })
                    .await;
                info!("do_connect: sent InboxFetch for blind inbox");
            }
        }

        info!("do_connect: done");
        Ok(())
    }

    fn spawn_io_task(
        &self,
        session: TransportSession,
        outbound_rx: mpsc::Receiver<OutboundCmd>,
        outbound_tx: mpsc::Sender<OutboundCmd>,
    ) {
        let event_tx = self.event_tx.clone();
        let pending = Arc::clone(&self.pending);
        let keys = Arc::clone(&self.keys);
        let pending_sends = Arc::clone(&self.pending_sends);
        let active_recvs = Arc::clone(&self.active_recvs);
        let pending_metas = Arc::clone(&self.pending_metas);
        let active_sends = Arc::clone(&self.active_sends);
        let ice_agent = Arc::clone(&self.ice_agent);
        let message_store = self.message_store.clone();
        tokio::spawn(async move {
            run_io_loop(
                session,
                outbound_rx,
                outbound_tx,
                event_tx,
                pending,
                keys,
                pending_sends,
                active_recvs,
                pending_metas,
                active_sends,
                ice_agent,
                message_store,
            )
            .await;
        });
    }

    /// Ask the server to create a new share link. Returns the link ID string.
    pub async fn create_link(&self) -> Result<String> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(PendingKind::CreateLink, tx);

        let envelope = serde_json::json!({ "action": "create_link" });
        self.send_raw(Bytes::from(envelope.to_string()), FrameFlags::NONE)
            .await?;

        let resp = rx
            .await
            .map_err(|_| Error::Session("create_link cancelled".into()))?;
        resp.get("link_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| Error::Protocol("missing link_id in response".into()))
    }

    /// Join a share link and return the remote peer's [`PeerId`].
    pub async fn join_link(&self, link: &str) -> Result<PeerId> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(PendingKind::JoinLink, tx);

        let msg = cypher_proto::SignalRequestPeer {
            link_id: link.to_string(),
        };
        self.send_raw(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;

        let resp = rx
            .await
            .map_err(|_| Error::Session("join_link cancelled".into()))?;

        if resp.get("found").and_then(|v| v.as_bool()) != Some(true) {
            let err = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("link not found");
            return Err(Error::Protocol(err.to_string()));
        }

        let hex = resp
            .get("peer_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Protocol("missing peer_id in response".into()))?;
        let bytes = hex_decode(hex)?;
        PeerId::from_bytes(&bytes).ok_or_else(|| Error::Protocol("invalid peer_id".into()))
    }

    /// Establish an E2EE session with `peer_id` as the X3DH initiator.
    ///
    /// Fetches the peer's prekeys from the signaling service, performs X3DH,
    /// and initialises the Double-Ratchet sender state.  After this call,
    /// [`send_message`](ClientApi::send_message) works for `peer_id`.
    pub async fn initiate_session(&self, peer_id: &PeerId) -> Result<()> {
        // Skip if a ratchet session already exists for this peer.
        if self.keys.has_session(peer_id.as_bytes()) {
            debug!(peer_id = %peer_id, "session already exists, skipping initiate");
            return Ok(());
        }

        let (tx, rx) = oneshot::channel();
        self.pending.insert(PendingKind::GetPrekeys, tx);

        let msg = cypher_proto::KeysGetPrekeys {
            peer_id: peer_id.to_vec(),
        };
        self.send_raw(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;

        let resp = rx
            .await
            .map_err(|_| Error::Session("get_prekeys cancelled".into()))?;

        if resp.get("found").and_then(|v| v.as_bool()) != Some(true) {
            let err = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("prekeys not found");
            return Err(Error::Protocol(err.to_string()));
        }

        let ik_dh_bytes = json_bytes_field(&resp, "identity_key")?;
        let spk_bytes = json_bytes_field(&resp, "signed_prekey")?;

        // Extract peer's inbox_id if present (for offline message delivery).
        let peer_inbox_id = resp
            .get("inbox_id")
            .and_then(|v| {
                if v.is_null() {
                    return None;
                }
                // inbox_id comes as a hex string from signaling
                v.as_str().and_then(|hex| hex_decode(hex).ok())
            });

        let their_ik_dh = X25519PublicKey::from(
            <[u8; 32]>::try_from(ik_dh_bytes.as_slice())
                .map_err(|_| Error::Crypto("identity_key must be 32 bytes".into()))?,
        );
        let their_spk = X25519PublicKey::from(
            <[u8; 32]>::try_from(spk_bytes.as_slice())
                .map_err(|_| Error::Crypto("signed_prekey must be 32 bytes".into()))?,
        );

        let shared_secret = cypher_crypto::x3dh::x3dh_mutual(
            self.keys.identity(),
            &self.keys.spk_secret(),
            &their_ik_dh,
            &their_spk,
        );

        // Deterministic role: lower peer_id = sender, higher = receiver.
        // This ensures both sides agree on who is Alice (sender) and Bob (receiver)
        // in the Double Ratchet, so the ratchet chains are symmetric.
        let our_id = self.session.peer_id().as_bytes();
        let their_id = peer_id.as_bytes();
        if our_id < their_id {
            // We are the sender (Alice): init with their SPK as ratchet public key.
            self.keys
                .init_sender_session(peer_id.as_bytes(), &shared_secret, their_spk);
            info!(peer_id = %peer_id, role = "sender", "mutual key agreement session initialised");
        } else {
            // We are the receiver (Bob): init with our own SPK secret.
            self.keys
                .init_receiver_session(peer_id.as_bytes(), &shared_secret);
            info!(peer_id = %peer_id, role = "receiver", "mutual key agreement session initialised");
        }

        // Persist the peer's inbox_id for future offline message delivery.
        if let (Some(inbox), Some(store)) = (&peer_inbox_id, &self.message_store) {
            if let Err(e) = store.save_peer_inbox_id(peer_id, inbox) {
                warn!(peer_id = %peer_id, error = %e, "failed to persist peer inbox_id");
            }
        }

        Ok(())
    }

    /// Return a reference to the message store, if one was configured.
    pub fn message_store(&self) -> Option<&Arc<dyn MessageStore>> {
        self.message_store.as_ref()
    }

    /// Fetch queued offline messages from our blind inbox.
    ///
    /// Sends `InboxFetch` with our inbox_id; the signaling service returns all
    /// queued messages and clears the inbox. Returned messages are dispatched
    /// through the normal I/O loop as proto payloads.
    pub async fn fetch_inbox(&self) -> Result<()> {
        if self.inbox_id.is_empty() {
            return Ok(());
        }
        let msg = cypher_proto::InboxFetch {
            inbox_id: self.inbox_id.clone(),
        };
        self.send_raw(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        debug!("sent InboxFetch");
        Ok(())
    }

    /// Store a message in a peer's blind inbox for offline delivery.
    ///
    /// The `ciphertext` must already be E2EE-encrypted. The server only sees
    /// the inbox_id (unlinkable to peer_id) and opaque ciphertext.
    pub async fn send_to_inbox(&self, inbox_id: &[u8], ciphertext: &[u8]) -> Result<()> {
        let msg = cypher_proto::InboxStore {
            inbox_id: inbox_id.to_vec(),
            ciphertext: ciphertext.to_vec(),
        };
        self.send_raw(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        debug!("sent InboxStore");
        Ok(())
    }

    /// Encrypt and send a message to `peer_id`.
    ///
    /// Requires a session to be established via
    /// [`initiate_session`](ClientApi::initiate_session).
    pub async fn send_message(&self, peer_id: &PeerId, plaintext: &[u8]) -> Result<()> {
        let (ciphertext, ratchet_key_bytes, msg_no) =
            self.keys.encrypt_for_peer(peer_id.as_bytes(), plaintext)?;

        let msg = cypher_proto::ChatSend {
            peer_id: peer_id.to_vec(),
            ciphertext,
            ratchet_key: ratchet_key_bytes,
            msg_no,
        };
        self.send_raw(Bytes::from(msg.serialize()), FrameFlags::NONE)
            .await?;
        debug!(peer_id = %peer_id, msg_no, "sent encrypted message");

        // Persist the outgoing message and ratchet state.
        if let Some(store) = &self.message_store {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if let Err(e) =
                store.save_message(peer_id, crate::persistence::Direction::Sent, plaintext, now)
            {
                warn!(peer_id = %peer_id, error = %e, "failed to persist sent message — message was sent but may not appear in history after restart");
            }
            if let Some(state) = self.keys.get_ratchet_state(peer_id.as_bytes()) {
                if let Err(e) = store.save_ratchet_state(peer_id, &state) {
                    warn!(peer_id = %peer_id, error = %e, "failed to persist ratchet state — session may break after restart");
                }
            }
        }

        Ok(())
    }

    /// Offer a file to `peer_id`.
    ///
    /// Opens and hashes the file, sends a `FileOffer` proto, stores the
    /// chunker so that when `FileAccept` arrives the background task can start
    /// streaming chunks.  Returns the `FileMeta` so the UI can display
    /// transfer progress.
    pub async fn send_file(&self, peer_id: &PeerId, path: &Path) -> Result<FileMeta> {
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
    ///
    /// `dest_path` is the local path where the assembled file will be written.
    /// Sends `FileAccept` to the sender; chunks will arrive as background
    /// events (`FileProgress`, `FileComplete`).
    pub async fn accept_file(&self, file_id: &[u8], dest_path: &Path) -> Result<()> {
        let (meta, sender_peer_id) = self
            .pending_metas
            .remove(file_id)
            .map(|(_, v)| v)
            .ok_or_else(|| Error::Protocol("no pending file offer for that id".into()))?;

        // Try to resume a partially received file.
        let (assembler, missing) = match FileAssembler::load_state(dest_path, &meta).await? {
            Some(asm) => {
                let m = asm.missing_chunks();
                (asm, Some(m))
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
            // Resume: send FileResume with packed missing indices.
            let packed: Vec<u8> = missing.iter().flat_map(|i| i.to_le_bytes()).collect();
            let msg = cypher_proto::FileResume {
                peer_id: sender_peer_id.to_vec(),
                file_id: file_id.to_vec(),
                missing: packed,
            };
            self.send_raw(Bytes::from(msg.serialize()), FrameFlags::NONE)
                .await?;
            info!(file_id = ?file_id, "FileResume sent (resume)");
        } else {
            // Normal accept.
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

    /// Gather ICE candidates and send them to the remote peer via signaling.
    ///
    /// `stun_server` is the address of the STUN server (e.g. "stun.example.com:3478").
    /// `peer_id` is the remote peer to exchange candidates with.
    pub async fn gather_candidates(
        &self,
        stun_server: SocketAddr,
        peer_id: &PeerId,
    ) -> Result<Vec<Candidate>> {
        let mut agent = IceAgent::new(stun_server).await?;
        let candidates = agent.gather_candidates().await?;

        // Send each candidate to the remote peer via signaling.
        for c in &candidates {
            let msg = cypher_proto::SignalIceCandidate {
                candidate: format!("{}", c.addr),
                peer_id: peer_id.to_vec(),
            };
            self.send_raw(Bytes::from(msg.serialize()), FrameFlags::NONE)
                .await?;
        }
        info!(count = candidates.len(), "ICE candidates gathered and sent");

        *self.ice_agent.lock().await = Some(agent);
        Ok(candidates)
    }

    /// Add a remote ICE candidate received from the peer.
    pub async fn add_remote_candidate(&self, candidate: Candidate) {
        if let Some(agent) = self.ice_agent.lock().await.as_mut() {
            agent.add_remote_candidate(candidate);
        }
    }

    /// Attempt to establish a direct P2P connection via ICE connectivity checks.
    ///
    /// Returns `(local_addr, remote_addr)` on success, or falls back to relay
    /// if all connectivity checks fail.
    pub async fn try_p2p_connect(&self) -> Result<(SocketAddr, SocketAddr)> {
        let mut guard = self.ice_agent.lock().await;
        let agent = guard
            .as_mut()
            .ok_or_else(|| Error::Nat("no ICE agent; call gather_candidates first".into()))?;

        let result = agent.check_connectivity().await;
        match result {
            Ok((local, remote)) => {
                let socket = Arc::clone(agent.socket());
                *self.p2p_socket.lock().await = Some(socket);
                info!(local = %local, remote = %remote, "P2P connection established");
                Ok((local, remote))
            }
            Err(e) => {
                warn!(error = %e, "P2P connectivity checks failed");
                Err(e)
            }
        }
    }

    /// Connect to a relay server as a fallback when P2P fails.
    ///
    /// `relay_addr` is the TCP address of the relay service.
    /// `session_key` identifies the relay session (shared with the remote peer).
    pub async fn connect_relay(&self, relay_addr: &str, session_key: &str) -> Result<()> {
        use cypher_nat::RelayClient;

        let client = RelayClient::connect(relay_addr, session_key).await?;
        info!(relay = relay_addr, "connected to relay (fallback)");
        *self.relay_client.lock().await = Some(client);
        Ok(())
    }

    /// Try P2P first; if it fails within `timeout`, fall back to relay.
    pub async fn connect_p2p_or_relay(
        &self,
        stun_server: SocketAddr,
        peer_id: &PeerId,
        relay_addr: &str,
        session_key: &str,
        timeout: Duration,
    ) -> Result<()> {
        // Gather and exchange candidates.
        self.gather_candidates(stun_server, peer_id).await?;

        // Give P2P a chance.
        match tokio::time::timeout(timeout, self.try_p2p_connect()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => {
                info!(error = %e, "P2P failed, falling back to relay");
                self.connect_relay(relay_addr, session_key).await
            }
            Err(_) => {
                info!("P2P timed out, falling back to relay");
                self.connect_relay(relay_addr, session_key).await
            }
        }
    }

    /// Poll for the next [`ClientEvent`]. Returns `None` on shutdown.
    pub async fn next_event(&self) -> Option<ClientEvent> {
        self.event_rx.lock().await.recv().await
    }

    async fn send_raw(&self, payload: Bytes, flags: FrameFlags) -> Result<()> {
        let guard = self.outbound_tx.lock().await;
        let tx = guard
            .as_ref()
            .ok_or_else(|| Error::Session("not connected to gateway".into()))?;
        tx.send(OutboundCmd::Send { payload, flags })
            .await
            .map_err(|_| Error::Session("outbound channel closed".into()))
    }
}

impl Default for ClientApi {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
async fn run_io_loop(
    mut session: TransportSession,
    mut outbound_rx: mpsc::Receiver<OutboundCmd>,
    outbound_tx: mpsc::Sender<OutboundCmd>,
    event_tx: mpsc::Sender<ClientEvent>,
    pending: Arc<DashMap<PendingKind, oneshot::Sender<serde_json::Value>>>,
    keys: Arc<KeyManager>,
    pending_sends: Arc<DashMap<Vec<u8>, (Arc<Mutex<FileChunker>>, PeerId, FileMeta)>>,
    active_recvs: Arc<DashMap<Vec<u8>, (Arc<Mutex<TransferReceiver>>, PeerId, bool)>>,
    pending_metas: Arc<DashMap<Vec<u8>, (FileMeta, PeerId)>>,
    active_sends: Arc<DashMap<Vec<u8>, mpsc::Sender<u32>>>,
    ice_agent: Arc<Mutex<Option<IceAgent>>>,
    message_store: Option<Arc<dyn MessageStore>>,
) {
    loop {
        tokio::select! {
            cmd = outbound_rx.recv() => match cmd {
                Some(OutboundCmd::Send { payload, flags }) => {
                    if let Err(e) = session.send_frame(payload, flags).await {
                        warn!("gateway write error: {}", e);
                        break;
                    }
                }
                Some(OutboundCmd::Close) | None => {
                    let _ = session.close().await;
                    break;
                }
            },

            result = session.recv_frame() => match result {
                Ok(frame) => {
                    dispatch_inbound(
                        frame.payload,
                        &event_tx,
                        &pending,
                        &keys,
                        &outbound_tx,
                        &pending_sends,
                        &active_recvs,
                        &pending_metas,
                        &active_sends,
                        &ice_agent,
                        &message_store,
                    )
                    .await;
                }
                Err(cypher_common::Error::ConnectionClosed) => {
                    info!("gateway connection closed");
                    break;
                }
                Err(e) => {
                    warn!("gateway read error: {}", e);
                    break;
                }
            },
        }
    }
    let _ = event_tx.send(ClientEvent::Disconnected).await;
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
async fn dispatch_inbound(
    payload: Bytes,
    event_tx: &mpsc::Sender<ClientEvent>,
    pending: &DashMap<PendingKind, oneshot::Sender<serde_json::Value>>,
    keys: &Arc<KeyManager>,
    outbound_tx: &mpsc::Sender<OutboundCmd>,
    pending_sends: &DashMap<Vec<u8>, (Arc<Mutex<FileChunker>>, PeerId, FileMeta)>,
    active_recvs: &DashMap<Vec<u8>, (Arc<Mutex<TransferReceiver>>, PeerId, bool)>,
    pending_metas: &DashMap<Vec<u8>, (FileMeta, PeerId)>,
    active_sends: &Arc<DashMap<Vec<u8>, mpsc::Sender<u32>>>,
    ice_agent: &Arc<Mutex<Option<IceAgent>>>,
    message_store: &Option<Arc<dyn MessageStore>>,
) {
    // JSON → signaling service response.
    if payload.first() == Some(&b'{') {
        match serde_json::from_slice::<serde_json::Value>(&payload) {
            Ok(json) => dispatch_json(json, pending, event_tx).await,
            Err(e) => warn!("malformed JSON from server: {}", e),
        }
        return;
    }

    // Binary → proto message from a peer.
    match dispatch(&payload) {
        Ok(Message::ChatSend(chat)) => {
            let Some(from) = PeerId::from_bytes(&chat.peer_id) else {
                warn!("ChatSend with invalid peer_id");
                return;
            };
            let rk_bytes: [u8; 32] = match chat.ratchet_key.as_slice().try_into() {
                Ok(b) => b,
                Err(_) => {
                    warn!("ChatSend ratchet_key != 32 bytes");
                    return;
                }
            };
            match keys.decrypt_from_peer(&chat.peer_id, &chat.ciphertext, &rk_bytes, chat.msg_no) {
                Ok(plaintext) => {
                    // Persist incoming message and ratchet state.
                    if let Some(ref store) = message_store {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        if let Err(e) = store.save_message(
                            &from,
                            crate::persistence::Direction::Received,
                            &plaintext,
                            now,
                        ) {
                            warn!("failed to persist received message: {e}");
                        }
                        if let Some(state) = keys.get_ratchet_state(&chat.peer_id) {
                            if let Err(e) = store.save_ratchet_state(&from, &state) {
                                warn!("failed to persist ratchet state: {e}");
                            }
                        }
                    }
                    let _ = event_tx
                        .send(ClientEvent::MessageReceived { from, plaintext })
                        .await;
                }
                Err(e) => {
                    warn!("decryption failed: {}", e);
                    let _ = event_tx
                        .send(ClientEvent::Error(format!("decrypt: {e}")))
                        .await;
                }
            }
        }

        Ok(Message::FileOffer(offer)) => {
            let Some(from) = PeerId::from_bytes(&offer.peer_id) else {
                warn!("FileOffer with invalid sender peer_id");
                return;
            };
            let meta = FileMeta {
                file_id: FileId::from_bytes(&offer.file_id).unwrap_or_else(FileId::generate),
                name: offer.name,
                size: offer.size,
                chunk_count: offer.chunks,
                hash: bytes::Bytes::from(offer.hash),
                compressed: offer.compressed != 0,
            };
            pending_metas.insert(offer.file_id, (meta.clone(), from.clone()));
            let _ = event_tx.send(ClientEvent::FileOffered { from, meta }).await;
        }

        Ok(Message::FileAccept(accept)) => {
            let Some(entry) = pending_sends.remove(&accept.file_id) else {
                warn!("FileAccept for unknown file_id");
                return;
            };
            let (chunker_arc, peer_id, meta) = entry.1;
            let file_id = accept.file_id.clone();

            let (ack_tx, ack_rx) = mpsc::channel::<u32>(DEFAULT_WINDOW_SIZE * 2);
            active_sends.insert(file_id.clone(), ack_tx);

            let tx = outbound_tx.clone();
            let ev_tx = event_tx.clone();
            let keys = Arc::clone(keys);
            let sends = Arc::clone(active_sends);
            tokio::spawn(async move {
                send_chunks(
                    chunker_arc,
                    file_id,
                    peer_id,
                    meta,
                    tx,
                    ev_tx,
                    keys,
                    ack_rx,
                    sends,
                    None,
                )
                .await;
            });
        }

        Ok(Message::FileChunk(chunk)) => {
            // Clone Arc and PeerId out of the DashMap Ref immediately so the
            // Ref is dropped before any await point (Rust borrow rules).
            let pair = active_recvs.get(&chunk.file_id).map(|e| {
                let (recv_arc, pid, compressed) = e.value();
                (Arc::clone(recv_arc), pid.clone(), *compressed)
            });
            let Some((recv_arc, sender_peer_id, is_compressed)) = pair else {
                warn!("FileChunk for unknown file_id");
                return;
            };

            // Decrypt chunk data (hash is plaintext, verified inside handle_chunk).
            let decrypted = match keys.decrypt_from_peer(
                sender_peer_id.as_bytes(),
                &chunk.data,
                &chunk.ratchet_key,
                chunk.msg_no,
            ) {
                Ok(pt) => pt,
                Err(e) => {
                    warn!("chunk decrypt failed: {}", e);
                    let _ = ev_tx_err(event_tx, format!("chunk decrypt: {e}")).await;
                    return;
                }
            };

            // Decompress if the transfer uses compression.
            let plaintext = if is_compressed {
                match cypher_transfer::decompress_chunk(&decrypted) {
                    Ok(decompressed) => decompressed,
                    Err(e) => {
                        warn!("chunk decompress failed: {}", e);
                        let _ = ev_tx_err(event_tx, format!("chunk decompress: {e}")).await;
                        return;
                    }
                }
            } else {
                decrypted
            };

            let done = {
                let mut recv = recv_arc.lock().await;
                match recv
                    .handle_chunk(chunk.index, &plaintext, &chunk.hash)
                    .await
                {
                    Ok(complete) => complete,
                    Err(e) => {
                        warn!("chunk write failed: {}", e);
                        let _ = ev_tx_err(event_tx, format!("chunk error: {e}")).await;
                        return;
                    }
                }
            };
            // Ack back to the sender.
            let ack = cypher_proto::FileChunkAck {
                peer_id: sender_peer_id.to_vec(),
                file_id: chunk.file_id.clone(),
                index: chunk.index,
            };
            let _ = outbound_tx
                .send(OutboundCmd::Send {
                    payload: Bytes::from(ack.serialize()),
                    flags: FrameFlags::NONE,
                })
                .await;

            let progress = recv_arc.lock().await.progress();
            let _ = event_tx
                .send(ClientEvent::FileProgress {
                    file_id: chunk.file_id.clone(),
                    progress,
                })
                .await;

            if done {
                let (verified, recv_clone) = {
                    let recv = recv_arc.lock().await;
                    (recv.verify().await, recv_arc.clone())
                };
                active_recvs.remove(&chunk.file_id);
                match verified {
                    Ok(true) => {
                        // Cleanup resume state file on success.
                        recv_clone.lock().await.cleanup_state().await;
                        let _ = event_tx
                            .send(ClientEvent::FileComplete {
                                file_id: chunk.file_id,
                            })
                            .await;
                    }
                    Ok(false) => {
                        let _ =
                            ev_tx_err(event_tx, "file integrity verification failed".to_string())
                                .await;
                    }
                    Err(e) => {
                        let _ = ev_tx_err(event_tx, format!("file verification error: {e}")).await;
                    }
                }
            }
        }

        Ok(Message::FileChunkAck(ack)) => {
            if let Some(sender) = active_sends.get(&ack.file_id) {
                let _ = sender.send(ack.index).await;
            }
        }

        Ok(Message::FileResume(resume)) => {
            let Some(entry) = pending_sends.remove(&resume.file_id) else {
                warn!("FileResume for unknown file_id");
                return;
            };
            let (chunker_arc, peer_id, meta) = entry.1;
            let file_id = resume.file_id.clone();

            // Decode the packed missing indices.
            let missing: Vec<u32> = resume
                .missing
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .collect();

            let (ack_tx, ack_rx) = mpsc::channel::<u32>(DEFAULT_WINDOW_SIZE * 2);
            active_sends.insert(file_id.clone(), ack_tx);

            let tx = outbound_tx.clone();
            let ev_tx = event_tx.clone();
            let keys = Arc::clone(keys);
            let sends = Arc::clone(active_sends);
            tokio::spawn(async move {
                send_chunks(
                    chunker_arc,
                    file_id,
                    peer_id,
                    meta,
                    tx,
                    ev_tx,
                    keys,
                    ack_rx,
                    sends,
                    Some(missing),
                )
                .await;
            });
        }

        Ok(Message::FileComplete(complete)) => {
            active_recvs.remove(&complete.file_id);
            let _ = event_tx
                .send(ClientEvent::FileComplete {
                    file_id: complete.file_id,
                })
                .await;
        }

        Ok(Message::SignalIceCandidate(ice)) => {
            let candidate_str = &ice.candidate;
            match candidate_str.parse::<SocketAddr>() {
                Ok(addr) => {
                    let candidate = Candidate::server_reflexive(addr);
                    debug!(addr = %addr, "received remote ICE candidate");
                    // Auto-add to ICE agent for connectivity checks.
                    if let Some(agent) = ice_agent.lock().await.as_mut() {
                        agent.add_remote_candidate(candidate.clone());
                    }
                    let _ = event_tx
                        .send(ClientEvent::IceCandidateReceived {
                            from: ice.peer_id,
                            candidate,
                        })
                        .await;
                }
                Err(e) => {
                    warn!(candidate = %candidate_str, error = %e, "invalid ICE candidate address");
                }
            }
        }

        Ok(Message::InboxMessages(inbox)) => {
            // Each message in the blob is length-prefixed: [u32 len][bytes]...
            // Each inner payload is a serialized ChatSend proto.
            let mut offset = 0usize;
            let blob = &inbox.messages;
            let mut delivered = 0u32;
            while offset + 4 <= blob.len() {
                let len = u32::from_le_bytes(blob[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                if offset + len > blob.len() {
                    break;
                }
                let msg_bytes = &blob[offset..offset + len];
                offset += len;

                // Each stored message is a serialized ChatSend.
                match dispatch(msg_bytes) {
                    Ok(Message::ChatSend(chat)) => {
                        let Some(from) = PeerId::from_bytes(&chat.peer_id) else {
                            continue;
                        };
                        let rk_bytes: [u8; 32] = match chat.ratchet_key.as_slice().try_into() {
                            Ok(b) => b,
                            Err(_) => continue,
                        };
                        match keys.decrypt_from_peer(
                            &chat.peer_id,
                            &chat.ciphertext,
                            &rk_bytes,
                            chat.msg_no,
                        ) {
                            Ok(plaintext) => {
                                if let Some(ref store) = message_store {
                                    let now = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();
                                    let _ = store.save_message(
                                        &from,
                                        crate::persistence::Direction::Received,
                                        &plaintext,
                                        now,
                                    );
                                    if let Some(state) = keys.get_ratchet_state(&chat.peer_id) {
                                        let _ = store.save_ratchet_state(&from, &state);
                                    }
                                }
                                let _ = event_tx
                                    .send(ClientEvent::MessageReceived { from, plaintext })
                                    .await;
                                delivered += 1;
                            }
                            Err(e) => {
                                warn!("inbox message decrypt failed: {e}");
                            }
                        }
                    }
                    _ => {
                        debug!("inbox contained non-ChatSend message, skipping");
                    }
                }
            }
            if delivered > 0 {
                info!(delivered, "processed inbox messages");
            }
        }

        Ok(other) => debug!("unhandled proto message: {:?}", other),
        Err(e) => debug!("unknown binary frame: {}", e),
    }
}

/// Send chunks of a file to `peer_id` using windowed flow control, then send `FileComplete`.
///
/// If `selective_indices` is `Some`, only those chunk indices are sent (resume).
/// If `None`, all chunks are sent.
#[allow(clippy::too_many_arguments)]
async fn send_chunks(
    chunker_mu: Arc<Mutex<FileChunker>>,
    file_id: Vec<u8>,
    peer_id: PeerId,
    meta: FileMeta,
    outbound_tx: mpsc::Sender<OutboundCmd>,
    event_tx: mpsc::Sender<ClientEvent>,
    keys: Arc<KeyManager>,
    ack_rx: mpsc::Receiver<u32>,
    active_sends: Arc<DashMap<Vec<u8>, mpsc::Sender<u32>>>,
    selective_indices: Option<Vec<u32>>,
) {
    // Extract FileChunker from Arc<Mutex<>>.
    let chunker = match Arc::try_unwrap(chunker_mu) {
        Ok(mutex) => mutex.into_inner(),
        Err(_) => {
            warn!("chunker arc has multiple owners, cannot proceed with file send");
            let _ = event_tx
                .send(ClientEvent::Error(
                    "file send failed: chunker still referenced".into(),
                ))
                .await;
            return;
        }
    };

    let mut sender = TransferSender::new(chunker, DEFAULT_WINDOW_SIZE);

    // Build ChunkSendFn closure: optionally compress + encrypt + serialize + send.
    let fid = file_id.clone();
    let pid = peer_id.clone();
    let out_tx = outbound_tx.clone();
    let ev_tx = event_tx.clone();
    let compressed = meta.compressed;
    let send_fn: ChunkSendFn = Box::new(move |index, data, hash| {
        let keys = keys.clone();
        let pid = pid.clone();
        let fid = fid.clone();
        let tx = out_tx.clone();
        let ev_tx = ev_tx.clone();
        Box::pin(async move {
            // Optionally compress before encryption.
            let send_data = if compressed {
                cypher_transfer::compress_chunk(&data)?
            } else {
                data.to_vec()
            };
            let (ciphertext, ratchet_key, msg_no) =
                keys.encrypt_for_peer(pid.as_bytes(), &send_data)?;
            let chunk = cypher_proto::FileChunk {
                peer_id: pid.to_vec(),
                file_id: fid.clone(),
                index,
                data: ciphertext,
                hash,
                ratchet_key,
                msg_no,
            };
            tx.send(OutboundCmd::Send {
                payload: Bytes::from(chunk.serialize()),
                flags: FrameFlags::NONE,
            })
            .await
            .map_err(|_| cypher_common::Error::Session("outbound closed".into()))?;
            let _ = ev_tx
                .send(ClientEvent::FileProgress {
                    file_id: fid,
                    progress: -1.0, // approximate; real progress tracked by acks
                })
                .await;
            Ok(())
        })
    });

    let result = match selective_indices {
        Some(indices) => sender.run_selective(indices, send_fn, ack_rx).await,
        None => sender.run(send_fn, ack_rx).await,
    };

    // Cleanup: remove from active_sends.
    active_sends.remove(&file_id);

    if let Err(e) = result {
        warn!("windowed transfer failed: {}", e);
        let _ = event_tx
            .send(ClientEvent::Error(format!("transfer: {e}")))
            .await;
        return;
    }

    // Signal transfer done.
    let complete = cypher_proto::FileComplete {
        peer_id: peer_id.to_vec(),
        file_id: file_id.clone(),
    };
    let _ = outbound_tx
        .send(OutboundCmd::Send {
            payload: Bytes::from(complete.serialize()),
            flags: FrameFlags::NONE,
        })
        .await;

    info!(file = %meta.name, "file transfer complete");
    let _ = event_tx.send(ClientEvent::FileComplete { file_id }).await;
}

async fn ev_tx_err(event_tx: &mpsc::Sender<ClientEvent>, msg: String) {
    let _ = event_tx.send(ClientEvent::Error(msg)).await;
}

async fn dispatch_json(
    json: serde_json::Value,
    pending: &DashMap<PendingKind, oneshot::Sender<serde_json::Value>>,
    event_tx: &mpsc::Sender<ClientEvent>,
) {
    // `peer_joined` notifications are unsolicited — emit an event instead of
    // routing to a pending request.
    if json.get("peer_joined").and_then(|v| v.as_bool()) == Some(true) {
        if let Some(hex) = json.get("peer_id").and_then(|v| v.as_str()) {
            if let Ok(bytes) = hex_decode(hex) {
                if let Some(peer_id) = PeerId::from_bytes(&bytes) {
                    let _ = event_tx.send(ClientEvent::PeerConnected { peer_id }).await;
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

    if let Some((_, tx)) = pending.remove(&kind) {
        let _ = tx.send(json);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn json_bytes_field(obj: &serde_json::Value, field: &str) -> Result<Vec<u8>> {
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

fn hex_decode(hex: &str) -> Result<Vec<u8>> {
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
