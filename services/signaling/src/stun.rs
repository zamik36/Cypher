use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;
const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_RESPONSE: u16 = 0x0101;
const STUN_ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const STUN_HEADER_SIZE: usize = 20;

pub struct StunServer {
    pub(crate) socket: Arc<UdpSocket>,
}

impl StunServer {
    pub async fn bind(addr: SocketAddr) -> anyhow::Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        info!("STUN server listening on {}", addr);
        Ok(Self {
            socket: Arc::new(socket),
        })
    }

    pub async fn run(&self) -> ! {
        let mut buf = [0u8; 576];
        loop {
            let (len, from) = match self.socket.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("STUN recv error: {}", e);
                    continue;
                }
            };

            let data = &buf[..len];
            if data.len() < STUN_HEADER_SIZE {
                debug!(%from, "STUN: datagram too short ({}B), ignoring", len);
                continue;
            }

            let msg_type = u16::from_be_bytes([data[0], data[1]]);
            let cookie = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            if msg_type != STUN_BINDING_REQUEST || cookie != STUN_MAGIC_COOKIE {
                debug!(%from, msg_type, "STUN: ignoring non-Binding-Request");
                continue;
            }

            let transaction_id = &data[8..20];
            let response = match build_binding_response(transaction_id, from) {
                Some(r) => r,
                None => {
                    debug!(%from, "STUN: unsupported address family, ignoring");
                    continue;
                }
            };

            if let Err(e) = self.socket.send_to(&response, from).await {
                warn!(%from, "STUN send error: {}", e);
            } else {
                debug!(%from, "STUN: sent Binding Response");
            }
        }
    }
}

pub(crate) fn build_binding_response(transaction_id: &[u8], peer: SocketAddr) -> Option<Vec<u8>> {
    let x_port = peer.port() ^ ((STUN_MAGIC_COOKIE >> 16) as u16);

    let attr_value = match peer {
        SocketAddr::V4(v4) => {
            let addr_u32: u32 = u32::from(*v4.ip());
            let x_addr = addr_u32 ^ STUN_MAGIC_COOKIE;

            let mut v = Vec::with_capacity(8);
            v.push(0x00);
            v.push(0x01);
            v.extend_from_slice(&x_port.to_be_bytes());
            v.extend_from_slice(&x_addr.to_be_bytes());
            v
        }
        SocketAddr::V6(v6) => {
            let mut xor_key = [0u8; 16];
            xor_key[..4].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
            xor_key[4..16].copy_from_slice(transaction_id);

            let addr_bytes = v6.ip().octets();
            let mut x_addr = [0u8; 16];
            for i in 0..16 {
                x_addr[i] = addr_bytes[i] ^ xor_key[i];
            }

            let mut v = Vec::with_capacity(20);
            v.push(0x00);
            v.push(0x02);
            v.extend_from_slice(&x_port.to_be_bytes());
            v.extend_from_slice(&x_addr);
            v
        }
    };

    let attr_len = attr_value.len() as u16;
    let msg_attrs_len = 4 + attr_value.len();

    let mut msg = Vec::with_capacity(STUN_HEADER_SIZE + msg_attrs_len);
    msg.extend_from_slice(&STUN_BINDING_RESPONSE.to_be_bytes());
    msg.extend_from_slice(&(msg_attrs_len as u16).to_be_bytes());
    msg.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    msg.extend_from_slice(transaction_id);
    msg.extend_from_slice(&STUN_ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
    msg.extend_from_slice(&attr_len.to_be_bytes());
    msg.extend_from_slice(&attr_value);

    Some(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};

    const TXN: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

    fn decode_xor_mapped(resp: &[u8], txn: &[u8; 12]) -> SocketAddr {
        assert_eq!(
            u16::from_be_bytes([resp[0], resp[1]]),
            STUN_BINDING_RESPONSE
        );
        assert_eq!(&resp[8..20], txn);
        let attr = &resp[STUN_HEADER_SIZE..];
        assert_eq!(
            u16::from_be_bytes([attr[0], attr[1]]),
            STUN_ATTR_XOR_MAPPED_ADDRESS
        );
        let value = &attr[4..];
        let port = u16::from_be_bytes([value[2], value[3]]) ^ (STUN_MAGIC_COOKIE >> 16) as u16;
        let ip = match value[1] {
            0x01 => {
                let x = u32::from_be_bytes(value[4..8].try_into().unwrap()) ^ STUN_MAGIC_COOKIE;
                IpAddr::V4(Ipv4Addr::from(x))
            }
            _ => {
                let mut key = [0u8; 16];
                key[..4].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
                key[4..].copy_from_slice(txn);
                let mut octets = [0u8; 16];
                for i in 0..16 {
                    octets[i] = value[4 + i] ^ key[i];
                }
                IpAddr::V6(Ipv6Addr::from(octets))
            }
        };
        SocketAddr::new(ip, port)
    }

    #[test]
    fn ipv4_response_roundtrip() {
        let peer = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 100), 12345));
        let resp = build_binding_response(&TXN, peer).unwrap();
        assert_eq!(decode_xor_mapped(&resp, &TXN), peer);
    }

    #[test]
    fn ipv6_response_roundtrip() {
        let ip = Ipv6Addr::new(0x2001, 0x0db8, 0x85a3, 0, 0, 0x8a2e, 0x0370, 0x7334);
        let peer = SocketAddr::V6(SocketAddrV6::new(ip, 54321, 0, 0));
        let resp = build_binding_response(&TXN, peer).unwrap();
        assert_eq!(decode_xor_mapped(&resp, &TXN), peer);
    }

    #[tokio::test]
    async fn server_answers_binding_requests() {
        let server = StunServer::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let server_addr = server.socket.local_addr().unwrap();
        tokio::spawn(async move { server.run().await });

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut req = Vec::with_capacity(STUN_HEADER_SIZE);
        req.extend_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
        req.extend_from_slice(&0u16.to_be_bytes());
        req.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        req.extend_from_slice(&TXN);
        client.send_to(&req, server_addr).await.unwrap();

        let mut buf = [0u8; 128];
        let (n, _) = client.recv_from(&mut buf).await.unwrap();
        assert_eq!(
            decode_xor_mapped(&buf[..n], &TXN),
            client.local_addr().unwrap()
        );
    }
}
