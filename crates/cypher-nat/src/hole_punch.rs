//! UDP hole punching using a simple PUNCH / PUNCH_ACK protocol.
//!
//! Protocol:
//! 1. Send `PUNCH` magic bytes periodically to the remote address.
//! 2. When we receive `PUNCH`, respond with `PUNCH_ACK`.
//! 3. When we receive `PUNCH_ACK`, the hole is considered punched.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use cypher_common::{Error, Result};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

/// Magic bytes sent during hole punching.
const PUNCH_MAGIC: &[u8] = b"PUNCH";
/// Acknowledgement bytes confirming the hole is punched.
const PUNCH_ACK_MAGIC: &[u8] = b"PUNCH_ACK";

/// Interval between PUNCH packets.
const PUNCH_INTERVAL: Duration = Duration::from_millis(200);

/// UDP hole puncher that sends periodic probe packets and listens for
/// acknowledgements.
pub struct HolePuncher {
    socket: Arc<UdpSocket>,
}

impl HolePuncher {
    pub fn new(socket: Arc<UdpSocket>) -> Self {
        Self { socket }
    }

    /// Attempt to punch a UDP hole to `remote_addr` within `timeout`.
    ///
    /// Sends `PUNCH` packets periodically. When a `PUNCH` is received from the
    /// remote, responds with `PUNCH_ACK`. Returns `Ok(())` once a `PUNCH_ACK`
    /// is received (meaning the remote has seen our `PUNCH` and confirmed
    /// bidirectional connectivity).
    pub async fn punch(&self, remote_addr: SocketAddr, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;

        debug!(remote = %remote_addr, "starting hole punch");

        let mut punch_interval = tokio::time::interval(PUNCH_INTERVAL);
        punch_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut buf = [0u8; 64];

        loop {
            tokio::select! {
                _ = punch_interval.tick() => {
                    if tokio::time::Instant::now() >= deadline {
                        warn!(remote = %remote_addr, "hole punch timed out");
                        return Err(Error::Timeout);
                    }

                    // Send PUNCH probe.
                    if let Err(e) = self.socket.send_to(PUNCH_MAGIC, remote_addr).await {
                        debug!(error = %e, "failed to send PUNCH");
                    }
                }

                result = self.socket.recv_from(&mut buf) => {
                    let (len, from) = result?;

                    if from != remote_addr {
                        // Ignore packets from unexpected sources.
                        continue;
                    }

                    let data = &buf[..len];

                    if data == PUNCH_ACK_MAGIC {
                        info!(remote = %remote_addr, "hole punch succeeded (received ACK)");
                        return Ok(());
                    }

                    if data == PUNCH_MAGIC {
                        debug!(remote = %remote_addr, "received PUNCH, sending ACK");
                        // Respond with ACK.
                        if let Err(e) = self.socket.send_to(PUNCH_ACK_MAGIC, remote_addr).await {
                            debug!(error = %e, "failed to send PUNCH_ACK");
                        }
                    }
                }

                _ = tokio::time::sleep_until(deadline) => {
                    warn!(remote = %remote_addr, "hole punch timed out");
                    return Err(Error::Timeout);
                }
            }
        }
    }
}
