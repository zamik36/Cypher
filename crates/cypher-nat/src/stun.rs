//! Minimal STUN client implementing just enough of RFC 5389 to discover
//! our server-reflexive (external) address via a Binding Request.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use cypher_common::{Error, Result};
use tokio::net::UdpSocket;
use tracing::{debug, warn};

/// STUN magic cookie (RFC 5389 section 6).
pub const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;

/// STUN message types.
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_RESPONSE: u16 = 0x0101;

/// STUN attribute types.
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

const STUN_HEADER_SIZE: usize = 20;

pub struct StunClient {
    socket: UdpSocket,
}

impl StunClient {
    pub async fn new() -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        Ok(Self { socket })
    }

    pub fn from_socket(socket: UdpSocket) -> Self {
        Self { socket }
    }

    /// Send a STUN Binding Request to `server_addr` and return our
    /// server-reflexive address from the response.
    pub async fn binding_request(&self, server_addr: SocketAddr) -> Result<SocketAddr> {
        let transaction_id: [u8; 12] = rand::Rng::gen(&mut rand::thread_rng());
        let request = build_binding_request(transaction_id);

        // Send with up to 3 retries.
        let mut buf = [0u8; 576];
        let mut attempts = 0;
        let max_attempts = 3;

        loop {
            self.socket.send_to(&request, server_addr).await?;
            debug!(
                server = %server_addr,
                attempt = attempts + 1,
                "sent STUN Binding Request"
            );

            let timeout = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                self.socket.recv_from(&mut buf),
            )
            .await;

            match timeout {
                Ok(Ok((len, from))) => {
                    debug!(from = %from, len, "received STUN response");
                    return parse_binding_response(&buf[..len], &transaction_id);
                }
                Ok(Err(e)) => return Err(Error::Io(e)),
                Err(_) => {
                    attempts += 1;
                    if attempts >= max_attempts {
                        warn!("STUN request timed out after {} attempts", max_attempts);
                        return Err(Error::Timeout);
                    }
                }
            }
        }
    }

    pub fn socket(&self) -> &UdpSocket {
        &self.socket
    }

    /// Consume the client and return the underlying socket.
    pub fn into_socket(self) -> UdpSocket {
        self.socket
    }
}

fn build_binding_request(transaction_id: [u8; 12]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(STUN_HEADER_SIZE);

    // Message type: Binding Request.
    msg.extend_from_slice(&BINDING_REQUEST.to_be_bytes());
    // Message length: 0 (no attributes).
    msg.extend_from_slice(&0u16.to_be_bytes());
    // Magic cookie.
    msg.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    // Transaction ID (12 bytes).
    msg.extend_from_slice(&transaction_id);

    msg
}

/// Parse a STUN Binding Response and extract the mapped address.
pub fn parse_binding_response(data: &[u8], transaction_id: &[u8; 12]) -> Result<SocketAddr> {
    if data.len() < STUN_HEADER_SIZE {
        return Err(Error::Nat("STUN response too short".into()));
    }

    let msg_type = u16::from_be_bytes([data[0], data[1]]);
    if msg_type != BINDING_RESPONSE {
        return Err(Error::Nat(format!(
            "expected Binding Response (0x{:04x}), got 0x{:04x}",
            BINDING_RESPONSE, msg_type
        )));
    }

    let msg_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    let cookie = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    if cookie != STUN_MAGIC_COOKIE {
        return Err(Error::Nat("invalid STUN magic cookie".into()));
    }

    // Verify transaction ID.
    if &data[8..20] != transaction_id {
        return Err(Error::Nat("STUN transaction ID mismatch".into()));
    }

    // Parse attributes.
    let attr_data = &data[STUN_HEADER_SIZE..];
    if attr_data.len() < msg_len {
        return Err(Error::Nat("STUN response truncated".into()));
    }
    let attr_data = &attr_data[..msg_len];

    // Prefer XOR-MAPPED-ADDRESS, fall back to MAPPED-ADDRESS.
    let mut xor_result = None;
    let mut mapped_result = None;

    let mut offset = 0;
    while offset + 4 <= attr_data.len() {
        let attr_type = u16::from_be_bytes([attr_data[offset], attr_data[offset + 1]]);
        let attr_len = u16::from_be_bytes([attr_data[offset + 2], attr_data[offset + 3]]) as usize;
        let value_start = offset + 4;
        let value_end = value_start + attr_len;

        if value_end > attr_data.len() {
            break;
        }

        let value = &attr_data[value_start..value_end];

        match attr_type {
            ATTR_XOR_MAPPED_ADDRESS => {
                xor_result = Some(parse_xor_mapped_address(value, transaction_id)?);
            }
            ATTR_MAPPED_ADDRESS => {
                mapped_result = Some(parse_mapped_address(value)?);
            }
            _ => {
                // Skip unknown attributes.
            }
        }

        // Attributes are padded to 4-byte boundaries.
        let padded_len = (attr_len + 3) & !3;
        offset = value_start + padded_len;
    }

    xor_result
        .or(mapped_result)
        .ok_or_else(|| Error::Nat("no MAPPED-ADDRESS in STUN response".into()))
}

/// Parse an XOR-MAPPED-ADDRESS attribute value.
fn parse_xor_mapped_address(data: &[u8], transaction_id: &[u8; 12]) -> Result<SocketAddr> {
    // Format: [0x00][family: u8][x-port: u16][x-address: 4 or 16 bytes]
    if data.len() < 8 {
        return Err(Error::Nat("XOR-MAPPED-ADDRESS too short".into()));
    }

    let family = data[1];
    let x_port = u16::from_be_bytes([data[2], data[3]]);
    let port = x_port ^ (STUN_MAGIC_COOKIE >> 16) as u16;

    match family {
        0x01 => {
            // IPv4
            let x_addr = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            let addr = Ipv4Addr::from(x_addr ^ STUN_MAGIC_COOKIE);
            Ok(SocketAddr::new(addr.into(), port))
        }
        0x02 => {
            // IPv6: XOR with magic cookie + transaction ID (16 bytes total).
            if data.len() < 20 {
                return Err(Error::Nat("XOR-MAPPED-ADDRESS IPv6 too short".into()));
            }
            let mut xor_key = [0u8; 16];
            xor_key[..4].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
            xor_key[4..16].copy_from_slice(transaction_id);

            let mut addr_bytes = [0u8; 16];
            for i in 0..16 {
                addr_bytes[i] = data[4 + i] ^ xor_key[i];
            }
            let addr = Ipv6Addr::from(addr_bytes);
            Ok(SocketAddr::new(addr.into(), port))
        }
        _ => Err(Error::Nat(format!(
            "unknown address family: 0x{:02x}",
            family
        ))),
    }
}

/// Parse a MAPPED-ADDRESS attribute value (non-XOR).
fn parse_mapped_address(data: &[u8]) -> Result<SocketAddr> {
    if data.len() < 8 {
        return Err(Error::Nat("MAPPED-ADDRESS too short".into()));
    }

    let family = data[1];
    let port = u16::from_be_bytes([data[2], data[3]]);

    match family {
        0x01 => {
            let addr = Ipv4Addr::new(data[4], data[5], data[6], data[7]);
            Ok(SocketAddr::new(addr.into(), port))
        }
        0x02 => {
            if data.len() < 20 {
                return Err(Error::Nat("MAPPED-ADDRESS IPv6 too short".into()));
            }
            let mut addr_bytes = [0u8; 16];
            addr_bytes.copy_from_slice(&data[4..20]);
            let addr = Ipv6Addr::from(addr_bytes);
            Ok(SocketAddr::new(addr.into(), port))
        }
        _ => Err(Error::Nat(format!(
            "unknown address family: 0x{:02x}",
            family
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_binding_request() {
        let tid = [1u8; 12];
        let req = build_binding_request(tid);

        assert_eq!(req.len(), STUN_HEADER_SIZE);
        assert_eq!(u16::from_be_bytes([req[0], req[1]]), BINDING_REQUEST);
        assert_eq!(u16::from_be_bytes([req[2], req[3]]), 0); // length
        assert_eq!(
            u32::from_be_bytes([req[4], req[5], req[6], req[7]]),
            STUN_MAGIC_COOKIE
        );
        assert_eq!(&req[8..20], &tid);
    }

    #[test]
    fn test_parse_xor_mapped_address_ipv4() {
        let tid = [0u8; 12];
        // family=0x01, port=0x1234 XOR'd with top 16 bits of cookie
        let port: u16 = 0x1234;
        let x_port = port ^ (STUN_MAGIC_COOKIE >> 16) as u16;
        let addr = Ipv4Addr::new(192, 168, 1, 1);
        let addr_u32 = u32::from(addr);
        let x_addr = addr_u32 ^ STUN_MAGIC_COOKIE;

        let mut data = vec![0x00, 0x01]; // reserved + family
        data.extend_from_slice(&x_port.to_be_bytes());
        data.extend_from_slice(&x_addr.to_be_bytes());

        let result = parse_xor_mapped_address(&data, &tid).unwrap();
        assert_eq!(result.port(), port);
        assert_eq!(result.ip(), std::net::IpAddr::V4(addr));
    }
}
