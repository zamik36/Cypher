use std::net::SocketAddr;
use std::sync::Arc;

use cypher_common::Result;
use cypher_nat::{Candidate, IceAgent};
use tokio::net::UdpSocket;

/// A direct peer-to-peer UDP connection established via ICE / hole-punching.
pub struct P2PConnection {
    local_addr: SocketAddr,
    remote_addr: SocketAddr,
    socket: Arc<UdpSocket>,
}

impl P2PConnection {
    /// Gather local candidates using `stun_server`, add `remote_candidates`,
    /// run connectivity checks, and return the first working connection pair.
    pub async fn establish(
        stun_server: SocketAddr,
        remote_candidates: Vec<Candidate>,
    ) -> Result<Self> {
        let mut agent = IceAgent::new(stun_server).await?;

        // Gather our own candidates (host + server-reflexive).
        agent.gather_candidates().await?;

        // Register all remote candidates supplied by the peer via signaling.
        for candidate in remote_candidates {
            agent.add_remote_candidate(candidate);
        }

        // Run connectivity checks; returns the first working (local, remote) pair.
        let (local_addr, remote_addr) = agent.check_connectivity().await?;

        // Share ownership of the socket from the ice agent.
        let socket = Arc::clone(agent.socket());

        tracing::info!(
            local = %local_addr,
            remote = %remote_addr,
            "P2P connection established"
        );

        Ok(Self {
            local_addr,
            remote_addr,
            socket,
        })
    }

    /// Send a datagram to the remote peer.
    pub async fn send(&self, data: &[u8]) -> Result<()> {
        self.socket.send_to(data, self.remote_addr).await?;
        Ok(())
    }

    /// Receive a datagram into `buf`.
    ///
    /// Silently drops packets that arrive from any address other than the
    /// established remote peer.
    pub async fn recv(&self, buf: &mut [u8]) -> Result<usize> {
        loop {
            let (len, from) = self.socket.recv_from(buf).await?;
            if from == self.remote_addr {
                return Ok(len);
            }
            // Packet from unexpected source – discard and try again.
            tracing::trace!(from = %from, "dropping datagram from unexpected source");
        }
    }

    /// The local socket address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The remote peer address.
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote_addr
    }
}
