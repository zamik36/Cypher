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

mod files;
mod messaging;
mod network;
mod runtime;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use dashmap::DashMap;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{info, warn};

use cypher_common::{Error, FileMeta, PeerId, Result};
use cypher_nat::{Candidate, IceAgent};
use cypher_transport::{FrameFlags, TransportSession};

use crate::connection::ServerConnection;
use crate::crypto::KeyManager;
use crate::onion::bootstrap::TransportBootstrap;
use crate::onion::config::AnonymousTransportConfig;
use crate::onion::indicator::AnonymityLevel;
use crate::onion::service::AnonymousTransportService;
use crate::persistence::MessageStore;
use crate::session::ClientSession;
use crate::signaling::SignalingClient;
use runtime::{run_io_loop, RuntimeContext};

/// An event emitted by the P2P subsystem to the UI layer.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    Connected { peer_id: PeerId },
    Disconnected,
    MessageReceived { from: PeerId, plaintext: Vec<u8> },
    FileOffered { from: PeerId, meta: FileMeta },
    FileProgress { file_id: Vec<u8>, progress: f64 },
    FileComplete { file_id: Vec<u8> },
    PeerConnected { peer_id: PeerId },
    IceCandidateReceived { from: Vec<u8>, candidate: Candidate },
    AnonymityLevelChanged { level: AnonymityLevel },
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
    pending_sends:
        Arc<DashMap<Vec<u8>, (Arc<Mutex<cypher_transfer::FileChunker>>, PeerId, FileMeta)>>,
    active_recvs:
        Arc<DashMap<Vec<u8>, (Arc<Mutex<cypher_transfer::TransferReceiver>>, PeerId, bool)>>,
    pending_metas: Arc<DashMap<Vec<u8>, (FileMeta, PeerId)>>,
    active_sends: Arc<DashMap<Vec<u8>, mpsc::Sender<u32>>>,
    ice_agent: Arc<Mutex<Option<IceAgent>>>,
    p2p_socket: Mutex<Option<Arc<UdpSocket>>>,
    relay_client: Arc<Mutex<Option<cypher_nat::RelayClient>>>,
    message_store: Option<Arc<dyn MessageStore>>,
    inbox_id: Vec<u8>,
    anonymous_config: Mutex<AnonymousTransportConfig>,
    anonymous_service: Mutex<Option<Arc<AnonymousTransportService>>>,
}

impl ClientApi {
    const CONNECT_PHASE_TIMEOUT: Duration = Duration::from_secs(10);
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
            anonymous_config: Mutex::new(AnonymousTransportConfig::default()),
            anonymous_service: Mutex::new(None),
        }
    }

    pub fn inbox_id(&self) -> &[u8] {
        &self.inbox_id
    }

    pub fn peer_id(&self) -> &PeerId {
        self.session.peer_id()
    }

    pub fn keys(&self) -> &Arc<KeyManager> {
        &self.keys
    }

    /// Update anonymous transport settings.
    pub async fn set_anonymous_transport_config(
        &self,
        config: AnonymousTransportConfig,
    ) -> Result<()> {
        *self.anonymous_config.lock().await = config.clone();
        if let Some(service) = self.anonymous_service.lock().await.clone() {
            service.set_config(config).await;
        }
        Ok(())
    }

    /// Connect to the gateway over TLS.
    pub async fn connect_to_gateway(&self, addr: &str) -> Result<()> {
        #[cfg(debug_assertions)]
        let tls_config = cypher_tls::make_client_config_insecure();
        #[cfg(not(debug_assertions))]
        let tls_config = cypher_tls::make_client_config();
        self.do_connect(addr, tls_config).await
    }

    /// Connect to the gateway with a specific [`rustls::ClientConfig`].
    pub async fn connect_to_gateway_with_config(
        &self,
        addr: &str,
        tls_config: Arc<rustls::ClientConfig>,
    ) -> Result<()> {
        self.do_connect(addr, tls_config).await
    }

    async fn do_connect(&self, addr: &str, tls_config: Arc<rustls::ClientConfig>) -> Result<()> {
        info!(addr, "do_connect: starting TLS connection...");
        let conn = ServerConnection::connect_tls(addr, tls_config.clone()).await?;
        info!(addr, peer_id = %self.session.peer_id(), "do_connect: TLS established");

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
            Ok(Err(error)) => {
                warn!("do_connect: SESSION_INIT failed: {error}");
                return Err(error);
            }
            Err(_) => {
                warn!("do_connect: SESSION_INIT timed out after 10s");
                return Err(Error::Transport("SESSION_INIT timed out".into()));
            }
        }

        let bundle = self.keys.key_bundle();
        let raw = bundle.to_bytes();
        info!("do_connect: uploading prekeys...");
        match tokio::time::timeout(
            Self::CONNECT_PHASE_TIMEOUT,
            signaling.upload_prekeys(
                raw[32..64].to_vec(),
                raw[64..96].to_vec(),
                self.inbox_id.clone(),
            ),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                warn!("do_connect: prekey upload failed: {error}");
                return Err(error);
            }
            Err(_) => {
                warn!("do_connect: prekey upload timed out after 10s");
                return Err(Error::Transport("prekey upload timed out".into()));
            }
        }
        info!("do_connect: prekeys uploaded");

        let bootstrap = match tokio::time::timeout(
            Self::CONNECT_PHASE_TIMEOUT,
            signaling.get_transport_bootstrap(),
        )
        .await
        {
            Ok(Ok(info)) => TransportBootstrap::from_proto(info)?,
            Ok(Err(error)) => {
                warn!("do_connect: transport bootstrap failed: {error}");
                return Err(error);
            }
            Err(_) => {
                warn!("do_connect: transport bootstrap timed out after 10s");
                return Err(Error::Transport("transport bootstrap timed out".into()));
            }
        };
        let anonymous_config = self.anonymous_config.lock().await.clone();
        let anonymous_service = Arc::new(AnonymousTransportService::new(
            addr.to_string(),
            tls_config.clone(),
            bootstrap,
            anonymous_config,
        )?);
        anonymous_service.start().await;
        *self.anonymous_service.lock().await = Some(anonymous_service.clone());

        let (outbound_tx, outbound_rx) = mpsc::channel::<OutboundCmd>(256);
        *self.outbound_tx.lock().await = Some(outbound_tx.clone());
        self.spawn_io_task(signaling.conn.into_session(), outbound_rx, outbound_tx);

        let _ = self
            .event_tx
            .send(ClientEvent::Connected {
                peer_id: self.session.peer_id().clone(),
            })
            .await;

        let level = anonymous_service.level().await;
        let _ = self
            .event_tx
            .send(ClientEvent::AnonymityLevelChanged { level })
            .await;

        if !self.inbox_id.is_empty() {
            match tokio::time::timeout(Self::CONNECT_PHASE_TIMEOUT, self.fetch_inbox()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    warn!("do_connect: blind inbox fetch failed: {error}");
                    return Err(error);
                }
                Err(_) => {
                    warn!("do_connect: blind inbox fetch timed out after 10s");
                    return Err(Error::Transport("blind inbox fetch timed out".into()));
                }
            }
            info!("do_connect: fetched blind inbox");
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
        let runtime = self.runtime_context(outbound_tx);
        tokio::spawn(async move {
            run_io_loop(session, outbound_rx, runtime).await;
        });
    }

    fn runtime_context(&self, outbound_tx: mpsc::Sender<OutboundCmd>) -> RuntimeContext {
        RuntimeContext {
            event_tx: self.event_tx.clone(),
            pending: Arc::clone(&self.pending),
            keys: Arc::clone(&self.keys),
            outbound_tx,
            pending_sends: Arc::clone(&self.pending_sends),
            active_recvs: Arc::clone(&self.active_recvs),
            pending_metas: Arc::clone(&self.pending_metas),
            active_sends: Arc::clone(&self.active_sends),
            ice_agent: Arc::clone(&self.ice_agent),
            message_store: self.message_store.clone(),
        }
    }

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
