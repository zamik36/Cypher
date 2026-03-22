//! ICE candidate gathering and connectivity checking.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use cypher_common::{Error, Result};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

use crate::candidate::{sort_candidates, Candidate};
use crate::hole_punch::HolePuncher;
use crate::stun::StunClient;

/// ICE agent that gathers local candidates, accepts remote candidates,
/// and performs connectivity checks.
pub struct IceAgent {
    local_candidates: Vec<Candidate>,
    remote_candidates: Vec<Candidate>,
    socket: Arc<UdpSocket>,
    stun_server: SocketAddr,
}

impl IceAgent {
    pub async fn new(stun_server: SocketAddr) -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        Ok(Self {
            local_candidates: Vec::new(),
            remote_candidates: Vec::new(),
            socket: Arc::new(socket),
            stun_server,
        })
    }

    pub async fn gather_candidates(&mut self) -> Result<Vec<Candidate>> {
        self.local_candidates.clear();

        // Host candidate: the local address we are bound to.
        let local_addr = self.socket.local_addr()?;
        let host = Candidate::host(local_addr);
        debug!(addr = %local_addr, "gathered host candidate");
        self.local_candidates.push(host);

        // Server-reflexive candidate via STUN.
        match self.stun_binding_request().await {
            Ok(srflx_addr) => {
                info!(addr = %srflx_addr, "gathered server-reflexive candidate");
                let srflx = Candidate::server_reflexive(srflx_addr);
                self.local_candidates.push(srflx);
            }
            Err(e) => {
                warn!(error = %e, "failed to gather server-reflexive candidate");
            }
        }

        sort_candidates(&mut self.local_candidates);
        Ok(self.local_candidates.clone())
    }

    /// Add a remote candidate received from the peer via signaling.
    pub fn add_remote_candidate(&mut self, candidate: Candidate) {
        debug!(
            addr = %candidate.addr,
            kind = ?candidate.candidate_type,
            "added remote candidate"
        );
        self.remote_candidates.push(candidate);
    }

    /// Check connectivity with remote candidates by attempting hole
    /// punching against each one in priority order.
    ///
    /// Returns `(local_addr, remote_addr)` of the first working pair.
    pub async fn check_connectivity(&self) -> Result<(SocketAddr, SocketAddr)> {
        let local_addr = self.socket.local_addr()?;
        let puncher = HolePuncher::new(Arc::clone(&self.socket));

        let mut sorted_remote = self.remote_candidates.clone();
        sort_candidates(&mut sorted_remote);

        for candidate in &sorted_remote {
            debug!(
                remote = %candidate.addr,
                kind = ?candidate.candidate_type,
                "checking connectivity"
            );

            match puncher.punch(candidate.addr, Duration::from_secs(5)).await {
                Ok(()) => {
                    info!(
                        local = %local_addr,
                        remote = %candidate.addr,
                        "connectivity check succeeded"
                    );
                    return Ok((local_addr, candidate.addr));
                }
                Err(e) => {
                    debug!(
                        remote = %candidate.addr,
                        error = %e,
                        "connectivity check failed"
                    );
                }
            }
        }

        Err(Error::Nat("all connectivity checks failed".into()))
    }

    pub fn socket(&self) -> &Arc<UdpSocket> {
        &self.socket
    }

    pub fn local_candidates(&self) -> &[Candidate] {
        &self.local_candidates
    }

    pub fn remote_candidates(&self) -> &[Candidate] {
        &self.remote_candidates
    }

    /// Perform a STUN binding request using the agent's shared socket.
    async fn stun_binding_request(&self) -> Result<SocketAddr> {
        // We need a separate socket for STUN since the main socket is shared.
        // The StunClient will use its own socket.
        let client = StunClient::new().await?;
        client.binding_request(self.stun_server).await
    }
}
