//! Web Push to a client's push service (RFC 8030): a signal that its inbox
//! holds something, nothing else. The payload is a constant, encrypted to
//! the subscription (RFC 8291) so push services see only ciphertext, and
//! each request is signed with our VAPID key (RFC 8292).
//!
//! Endpoints come from anonymous clients, so this is an outbound request on
//! their behalf: only HTTPS on port 443 to known push services, and never
//! to an address inside a private network.

use std::net::IpAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint as _;
use p256::{EncodedPoint, PublicKey};
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// What the client's service worker or app receives: always the same.
const PAYLOAD: &[u8] = b"cypher:inbox";
/// How long a push service keeps a signal for an unreachable device.
const TTL_SECS: u32 = 24 * 3600;
/// Record size advertised in the aes128gcm header.
const RECORD_SIZE: u32 = 4096;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Push services that endpoints may point at (`*.` matches subdomains).
const KNOWN_SERVICES: &[&str] = &[
    "fcm.googleapis.com",
    "updates.push.services.mozilla.com",
    "*.notify.windows.com",
    "web.push.apple.com",
    "ntfy.sh",
];
use cypher_wire::MAX_PUSH_ENDPOINT_LEN as MAX_ENDPOINT_LEN;

/// A browser's or phone's push subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Subscription {
    pub endpoint: String,
    pub p256dh: [u8; 65],
    pub auth: [u8; 16],
}

impl Subscription {
    /// `p256dh ‖ auth ‖ endpoint`, as kept in Redis.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        [&self.p256dh[..], &self.auth, self.endpoint.as_bytes()].concat()
    }

    pub(crate) fn from_bytes(raw: &[u8]) -> Option<Self> {
        let (p256dh, rest) = raw.split_first_chunk::<65>()?;
        let (auth, endpoint) = rest.split_first_chunk::<16>()?;
        Some(Self {
            endpoint: String::from_utf8(endpoint.to_vec()).ok()?,
            p256dh: *p256dh,
            auth: *auth,
        })
    }
}

/// How a push service answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Sent,
    /// The subscription no longer exists (404/410): forget it.
    Gone,
    Failed,
}

/// Where endpoints may point, and how strictly.
#[derive(Debug, Clone)]
pub(crate) struct Policy {
    hosts: Vec<String>,
    /// Tests only: plain HTTP to loopback, any host.
    insecure_loopback: bool,
}

impl Policy {
    pub(crate) fn new(extra_hosts: &[String]) -> Self {
        let hosts = KNOWN_SERVICES
            .iter()
            .map(|h| (*h).to_owned())
            .chain(extra_hosts.iter().map(|h| h.trim().to_ascii_lowercase()))
            .filter(|h| !h.is_empty())
            .collect();
        Self {
            hosts,
            insecure_loopback: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn insecure_loopback() -> Self {
        Self {
            hosts: Vec::new(),
            insecure_loopback: true,
        }
    }

    /// The endpoint split into scheme-checked host, port and path, if it
    /// may be called at all.
    pub(crate) fn check(&self, endpoint: &str) -> Option<Target> {
        if endpoint.len() > MAX_ENDPOINT_LEN {
            return None;
        }
        let (tls, rest) = if let Some(rest) = endpoint.strip_prefix("https://") {
            (true, rest)
        } else if self.insecure_loopback {
            (false, endpoint.strip_prefix("http://")?)
        } else {
            return None;
        };
        let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
        if authority.contains('@') || path.chars().any(|c| c.is_ascii_control() || c == ' ') {
            return None;
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.contains(']') || h.ends_with(']') => (h, p.parse().ok()?),
            _ => (authority, if tls { 443 } else { 80 }),
        };
        let host = host.to_ascii_lowercase();
        if self.insecure_loopback {
            return (!tls && host == "127.0.0.1").then(|| Target::new(tls, host, port, path));
        }
        let allowed = port == 443
            && host.parse::<IpAddr>().is_err()
            && !host.starts_with('[')
            && self
                .hosts
                .iter()
                .any(|known| match known.strip_prefix("*.") {
                    Some(domain) => host
                        .strip_suffix(domain)
                        .is_some_and(|sub| sub.ends_with('.')),
                    None => host == *known,
                });
        allowed.then(|| Target::new(tls, host, port, path))
    }
}

/// A checked endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    tls: bool,
    host: String,
    port: u16,
    path: String,
}

impl Target {
    fn new(tls: bool, host: String, port: u16, path: &str) -> Self {
        Self {
            tls,
            host,
            port,
            path: format!("/{path}"),
        }
    }

    fn origin(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        match (self.tls, self.port) {
            (true, 443) | (false, 80) => format!("{scheme}://{}", self.host),
            _ => format!("{scheme}://{}:{}", self.host, self.port),
        }
    }
}

/// Our VAPID identity and the HTTPS client that sends signals.
pub(crate) struct WebPush {
    key: SigningKey,
    public: [u8; 65],
    contact: String,
    policy: Policy,
    tls: tokio_rustls::TlsConnector,
}

impl WebPush {
    /// `secret` is the 32-byte VAPID private key kept on disk.
    pub(crate) fn new(secret: [u8; 32], contact: String, policy: Policy) -> anyhow::Result<Self> {
        let key = SigningKey::from_bytes(&secret.into())?;
        let public = encoded(&PublicKey::from(key.verifying_key()).to_encoded_point(false));
        Ok(Self {
            key,
            public,
            contact,
            policy,
            tls: tokio_rustls::TlsConnector::from(cypher_tls::make_client_config()),
        })
    }

    /// The uncompressed P-256 public key clients subscribe with.
    pub(crate) fn public_key(&self) -> [u8; 65] {
        self.public
    }

    pub(crate) fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Sends the signal to `sub`.
    pub(crate) async fn send(&self, sub: &Subscription) -> Outcome {
        let Some(target) = self.policy.check(&sub.endpoint) else {
            return Outcome::Gone;
        };
        let Some(body) = encrypt(PAYLOAD, &sub.p256dh, &sub.auth) else {
            return Outcome::Gone;
        };
        let auth = self.authorization(&target.origin());
        match tokio::time::timeout(REQUEST_TIMEOUT, self.post(&target, &auth, &body)).await {
            Ok(Ok(status)) if (200..300).contains(&status) => Outcome::Sent,
            Ok(Ok(404 | 410)) => Outcome::Gone,
            Ok(Ok(status)) => {
                tracing::debug!(status, "push service refused the signal");
                Outcome::Failed
            }
            Ok(Err(e)) => {
                tracing::debug!("push request failed: {e}");
                Outcome::Failed
            }
            Err(_) => Outcome::Failed,
        }
    }

    /// `vapid t=<JWT>, k=<public key>` for `audience`.
    fn authorization(&self, audience: &str) -> String {
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            + 12 * 3600;
        let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = B64.encode(format!(
            r#"{{"aud":"{audience}","exp":{exp},"sub":"{}"}}"#,
            self.contact
        ));
        let signed = format!("{header}.{claims}");
        let signature: Signature = self.key.sign(signed.as_bytes());
        format!(
            "vapid t={signed}.{}, k={}",
            B64.encode(signature.to_bytes()),
            B64.encode(self.public)
        )
    }

    async fn post(&self, target: &Target, auth: &str, body: &[u8]) -> std::io::Result<u16> {
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

fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1])
                || v4.octets()[0] == 0)
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || first & 0xfe00 == 0xfc00
                || first & 0xffc0 == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| !public(IpAddr::V4(v4))))
        }
    }
}

fn encoded(point: &EncodedPoint) -> [u8; 65] {
    let mut out = [0u8; 65];
    out.copy_from_slice(point.as_bytes());
    out
}

/// `payload` encrypted to a subscription's keys (RFC 8291, aes128gcm).
fn encrypt(payload: &[u8], p256dh: &[u8; 65], auth: &[u8; 16]) -> Option<Vec<u8>> {
    let mut secret = [0u8; 32];
    let mut salt = [0u8; 16];
    loop {
        OsRng.fill_bytes(&mut secret);
        if p256::SecretKey::from_bytes(&secret.into()).is_ok() {
            break;
        }
    }
    OsRng.fill_bytes(&mut salt);
    encrypt_with(payload, p256dh, auth, &secret, &salt)
}

fn encrypt_with(
    payload: &[u8],
    p256dh: &[u8; 65],
    auth: &[u8; 16],
    sender_secret: &[u8; 32],
    salt: &[u8; 16],
) -> Option<Vec<u8>> {
    let receiver = PublicKey::from_sec1_bytes(p256dh).ok()?;
    let sender = p256::SecretKey::from_bytes(&(*sender_secret).into()).ok()?;
    let sender_public = encoded(&sender.public_key().to_encoded_point(false));
    let shared = p256::ecdh::diffie_hellman(sender.to_nonzero_scalar(), receiver.as_affine());

    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(p256dh);
    key_info.extend_from_slice(&sender_public);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), shared.raw_secret_bytes())
        .expand(&key_info, &mut ikm)
        .ok()?;
    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek)
        .ok()?;
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).ok()?;

    let mut record = payload.to_vec();
    record.push(2); // last (and only) record, no padding
    let ciphertext = Aes128Gcm::new(&cek.into())
        .encrypt(Nonce::from_slice(&nonce), record.as_slice())
        .ok()?;

    let mut body = Vec::with_capacity(16 + 4 + 1 + 65 + ciphertext.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(65);
    body.extend_from_slice(&sender_public);
    body.extend_from_slice(&ciphertext);
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> Vec<u8> {
        B64.decode(s).unwrap()
    }

    /// The worked example of RFC 8291, section 5.
    #[test]
    fn encrypts_like_the_rfc_example() {
        let p256dh: [u8; 65] = b64(
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
        )
        .try_into()
        .unwrap();
        let auth: [u8; 16] = b64("BTBZMqHH6r4Tts7J_aSIgg").try_into().unwrap();
        let sender: [u8; 32] = b64("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")
            .try_into()
            .unwrap();
        let salt: [u8; 16] = b64("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let body = encrypt_with(
            b"When I grow up, I want to be a watermelon",
            &p256dh,
            &auth,
            &sender,
            &salt,
        )
        .unwrap();
        assert_eq!(
            B64.encode(body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    #[test]
    fn endpoints_go_only_to_known_push_services() {
        let policy = Policy::new(&["push.example.org".into()]);
        let ok = |e: &str| policy.check(e).is_some();
        assert!(ok("https://fcm.googleapis.com/fcm/send/abc"));
        assert!(ok("https://updates.push.services.mozilla.com/wpush/v2/x"));
        assert!(ok("https://db5p.notify.windows.com/w/?token=1"));
        assert!(ok("https://ntfy.sh/upAbc?up=1"));
        assert!(ok("https://push.example.org/x"));
        assert!(ok("https://FCM.googleapis.com:443/x"));

        for bad in [
            "http://fcm.googleapis.com/x",
            "https://fcm.googleapis.com:8443/x",
            "https://evil.com/x",
            "https://notify.windows.com.evil.com/x",
            "https://xnotify.windows.com/x",
            "https://127.0.0.1/x",
            "https://[::1]/x",
            "https://10.0.0.5/x",
            "https://user@fcm.googleapis.com/x",
            "https://fcm.googleapis.com/a b",
            "ftp://fcm.googleapis.com/x",
            "fcm.googleapis.com/x",
        ] {
            assert!(!ok(bad), "{bad}");
        }
        assert!(!ok(&format!(
            "https://ntfy.sh/{}",
            "a".repeat(MAX_ENDPOINT_LEN)
        )));
        let target = policy.check("https://ntfy.sh/up1").unwrap();
        assert_eq!(
            (target.origin(), target.path.as_str()),
            ("https://ntfy.sh".into(), "/up1")
        );
    }

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
        ] {
            assert!(!public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["142.250.74.10", "2a00:1450:4001::200e"] {
            assert!(public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn subscriptions_round_trip() {
        let sub = Subscription {
            endpoint: "https://ntfy.sh/up1".into(),
            p256dh: [4; 65],
            auth: [9; 16],
        };
        assert_eq!(Subscription::from_bytes(&sub.to_bytes()), Some(sub));
        assert_eq!(Subscription::from_bytes(&[1; 70]), None);
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

        let device = p256::SecretKey::random(&mut OsRng);
        let p256dh = encoded(&device.public_key().to_encoded_point(false));
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
