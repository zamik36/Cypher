use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::TcpStream;

use super::policy::Target;
use super::{TTL_SECS, WebPush};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

impl WebPush {
    pub(super) async fn post(
        &self,
        target: &Target,
        auth: &str,
        body: &[u8],
    ) -> std::io::Result<u16> {
        let addr = resolve(&target.host, target.port, self.policy.insecure_loopback).await?;
        let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| std::io::ErrorKind::TimedOut)??;
        let head = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nAuthorization: {auth}\r\nTTL: {TTL_SECS}\r\n\
             Urgency: high\r\nContent-Encoding: aes128gcm\r\n\
             Content-Type: application/octet-stream\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            target.path,
            target.host,
            body.len()
        );
        if target.tls {
            let name = rustls_pki_types::ServerName::try_from(target.host.clone())
                .map_err(|_| std::io::ErrorKind::InvalidInput)?;
            exchange(self.tls.connect(name, tcp).await?, &head, body).await
        } else {
            exchange(tcp, &head, body).await
        }
    }
}

/// Writes the request and reads the status code of the response.
async fn exchange(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    head: &str,
    body: &[u8],
) -> std::io::Result<u16> {
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    let mut line = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    while line.len() < 256 && !line.ends_with(b"\r\n") {
        if stream.read(&mut byte).await? == 0 {
            break;
        }
        line.push(byte[0]);
    }
    std::str::from_utf8(&line)
        .ok()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| std::io::ErrorKind::InvalidData.into())
}

/// The first address of `host`, refusing any inside a private network so
/// an endpoint cannot make us call into our own infrastructure.
async fn resolve(
    host: &str,
    port: u16,
    loopback_ok: bool,
) -> std::io::Result<std::net::SocketAddr> {
    let mut addrs = tokio::net::lookup_host((host, port)).await?;
    let addr = addrs.next().ok_or(std::io::ErrorKind::NotFound)?;
    if public(addr.ip()) || (loopback_ok && addr.ip().is_loopback()) {
        Ok(addr)
    } else {
        Err(std::io::ErrorKind::PermissionDenied.into())
    }
}

/// Whether `ip` is an address on the public internet, not one inside a
/// private, special-purpose or reserved range.
fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => public_v6(v6),
    }
}

fn public_v4(v4: Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    !(v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || v4.is_documentation()
        // This network, 0.0.0.0/8.
        || a == 0
        // Shared address space (carrier-grade NAT), 100.64.0.0/10.
        || a == 100 && (64..128).contains(&b)
        // IETF protocol assignments, 192.0.0.0/24.
        || a == 192 && b == 0 && c == 0
        // 6to4 relay anycast, 192.88.99.0/24.
        || a == 192 && b == 88 && c == 99
        // Benchmarking, 198.18.0.0/15.
        || a == 198 && (b == 18 || b == 19)
        // Reserved, 240.0.0.0/4.
        || a >= 240)
}

/// An IPv6 address that carries an IPv4 one (mapped, NAT64, 6to4) is
/// public if that one is.
fn public_v6(v6: Ipv6Addr) -> bool {
    let v4 = |hi: u16, lo: u16| Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo));
    match v6.segments() {
        // IPv4-mapped ::ffff:0:0/96, NAT64 64:ff9b::/96, 6to4 2002::/16.
        [0, 0, 0, 0, 0, 0xffff, hi, lo]
        | [0x64, 0xff9b, 0, 0, 0, 0, hi, lo]
        | [0x2002, hi, lo, ..] => public_v4(v4(hi, lo)),
        // ::/64 (unspecified, loopback, IPv4-compatible and -translated),
        // discard-only 100::/64, NAT64 for local use 64:ff9b:1::/48, IETF
        // protocol assignments 2001::/23 (Teredo among them), and
        // documentation 2001:db8::/32 and 3fff::/20.
        [0 | 0x100, 0, 0, 0, ..]
        | [0x64, 0xff9b, 1, ..]
        | [0x2001, 0..=0x1ff | 0xdb8, ..]
        | [0x3fff, 0..=0xfff, ..] => false,
        [first, ..] => {
            !(v6.is_multicast()
                // Unique local, fc00::/7.
                || first & 0xfe00 == 0xfc00
                // Link local, fe80::/10, and the deprecated site local, fec0::/10.
                || first & 0xffc0 == 0xfe80
                || first & 0xffc0 == 0xfec0)
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
    use p256::ecdsa::Signature;

    use super::super::{Outcome, PAYLOAD, Policy, Subscription, encoded};
    use super::*;

    #[test]
    fn private_addresses_are_never_called() {
        for ip in [
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
            // Special-purpose ranges, and private IPv4 inside IPv6.
            "64:ff9b::a00:1",
            "64:ff9b:1::a00:1",
            "64:ff9b:1::8.8.8.8",
            "::a00:1",
            "::8.8.8.8",
            "::ffff:0:a00:1",
            "2002:a00:1::1",
            "2002:7f00:1::",
            "fec0::1",
            "100::1",
            "2001:db8::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2001:1ff::1",
            "3fff::1",
            "3fff:fff::1",
            "198.18.0.1",
            "198.19.255.255",
            "192.0.0.8",
            "240.0.0.1",
            "255.255.255.255",
            "192.88.99.1",
        ] {
            assert!(!public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "142.250.74.10",
            "2a00:1450:4001::200e",
            // A public IPv4 address behind NAT64 or 6to4 stays reachable.
            "64:ff9b::8.8.8.8",
            "2002:808:808::1",
            "::ffff:8.8.8.8",
            "198.20.0.1",
            "192.0.1.1",
            "2001:200::1",
            "3fff:1000::1",
        ] {
            assert!(public(ip.parse().unwrap()), "{ip}");
        }
    }

    /// Where the body of `request` starts, once its head is complete.
    fn body_start(request: &[u8]) -> Option<usize> {
        request
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|at| at + 4)
    }

    /// Whether `request` holds its whole body.
    fn complete(request: &[u8]) -> bool {
        let Some(at) = body_start(request) else {
            return false;
        };
        let head = String::from_utf8_lossy(&request[..at]);
        let len = head
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .and_then(|l| l.parse::<usize>().ok())
            .unwrap_or(0);
        request.len() - at >= len
    }

    /// A push service on loopback: answers `status` and hands back the request.
    async fn service(status: &'static str) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/push/abc", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
                if n == 0 || complete(&request) {
                    break;
                }
            }
            let reply = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
            stream.write_all(reply.as_bytes()).await.unwrap();
            request
        });
        (endpoint, task)
    }

    #[tokio::test]
    async fn signals_are_signed_encrypted_posts() {
        use p256::ecdsa::VerifyingKey;
        use p256::ecdsa::signature::Verifier as _;
        use p256::elliptic_curve::Generate as _;

        let device = p256::SecretKey::generate();
        let p256dh = encoded(&device.public_key());
        let web = WebPush::new(
            [5; 32],
            "mailto:ops@example.org".into(),
            Policy::insecure_loopback(),
        )
        .unwrap();
        let (endpoint, request) = service("201 Created").await;
        let sub = Subscription {
            endpoint: endpoint.clone(),
            p256dh,
            auth: [3; 16],
        };
        assert_eq!(web.send(&sub).await, Outcome::Sent);

        let request = request.await.unwrap();
        let at = body_start(&request).unwrap();
        let head = String::from_utf8_lossy(&request[..at]);
        assert!(head.starts_with("POST /push/abc HTTP/1.1\r\n"));
        assert!(head.contains("Content-Encoding: aes128gcm"));
        assert!(head.contains("TTL: 86400"));
        let auth = head
            .lines()
            .find_map(|l| l.strip_prefix("Authorization: vapid t="))
            .unwrap();
        let (jwt, key) = auth.split_once(", k=").unwrap();
        assert_eq!(B64.decode(key).unwrap(), web.public_key());
        let (signed, signature) = jwt.rsplit_once('.').unwrap();
        let verifying = VerifyingKey::from_sec1_bytes(&web.public_key()).unwrap();
        let signature = Signature::from_slice(&B64.decode(signature).unwrap()).unwrap();
        verifying.verify(signed.as_bytes(), &signature).unwrap();
        let claims =
            String::from_utf8(B64.decode(signed.split_once('.').unwrap().1).unwrap()).unwrap();
        let origin = endpoint.trim_end_matches("/push/abc");
        assert!(claims.contains(&format!(r#""aud":"{origin}""#)), "{claims}");
        // The body is RFC 8291 ciphertext: salt, record size, our key.
        let body = &request[at..];
        assert_eq!(&body[16..21], &[0, 0, 16, 0, 65]);
        assert!(!body.windows(PAYLOAD.len()).any(|w| w == PAYLOAD));

        let (endpoint, request) = service("410 Gone").await;
        let gone = Subscription { endpoint, ..sub };
        assert_eq!(web.send(&gone).await, Outcome::Gone);
        request.await.unwrap();
    }
}
