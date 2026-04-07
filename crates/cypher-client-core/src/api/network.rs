use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use cypher_common::{Error, PeerId, Result};
use cypher_nat::{Candidate, RelayClient};

use super::ClientApi;

impl ClientApi {
    /// Gather ICE candidates and send them to the remote peer via signaling.
    pub async fn gather_candidates(
        &self,
        stun_server: SocketAddr,
        peer_id: &PeerId,
    ) -> Result<Vec<Candidate>> {
        let mut agent = cypher_nat::IceAgent::new(stun_server).await?;
        let candidates = agent.gather_candidates().await?;

        for candidate in &candidates {
            let msg = cypher_proto::SignalIceCandidate {
                candidate: format!("{}", candidate.addr),
                peer_id: peer_id.to_vec(),
            };
            self.send_raw(
                bytes::Bytes::from(cypher_proto::Serializable::serialize(&msg)),
                cypher_transport::FrameFlags::NONE,
            )
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
    pub async fn try_p2p_connect(&self) -> Result<(SocketAddr, SocketAddr)> {
        let mut guard = self.ice_agent.lock().await;
        let agent = guard
            .as_mut()
            .ok_or_else(|| Error::Nat("no ICE agent; call gather_candidates first".into()))?;

        match agent.check_connectivity().await {
            Ok((local, remote)) => {
                let socket = Arc::clone(agent.socket());
                *self.p2p_socket.lock().await = Some(socket);
                info!(local = %local, remote = %remote, "P2P connection established");
                Ok((local, remote))
            }
            Err(error) => {
                warn!(error = %error, "P2P connectivity checks failed");
                Err(error)
            }
        }
    }

    /// Connect to a relay server as a fallback when P2P fails.
    pub async fn connect_relay(&self, relay_addr: &str, session_key: &str) -> Result<()> {
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
        self.gather_candidates(stun_server, peer_id).await?;

        match tokio::time::timeout(timeout, self.try_p2p_connect()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => {
                info!(error = %error, "P2P failed, falling back to relay");
                self.connect_relay(relay_addr, session_key).await
            }
            Err(_) => {
                info!("P2P timed out, falling back to relay");
                self.connect_relay(relay_addr, session_key).await
            }
        }
    }
}
