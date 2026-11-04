//! Web Push to a client's push service (RFC 8030): a signal that its inbox
//! holds something, nothing else. The payload is a constant, encrypted to
//! the subscription (RFC 8291) so push services see only ciphertext, and
//! each request is signed with our VAPID key (RFC 8292).
//!
//! Endpoints come from anonymous clients, so this is an outbound request on
//! their behalf: only HTTPS on port 443 to known push services, and never
//! to an address inside a private network.

use std::time::Duration;

use p256::PublicKey;
use p256::ecdsa::SigningKey;
use p256::elliptic_curve::sec1::ToSec1Point as _;

mod ece;
mod http;
mod policy;
mod vapid;

pub(crate) use policy::Policy;

use ece::encrypt;

/// What the client's service worker or app receives: always the same.
const PAYLOAD: &[u8] = b"cypher:inbox";
/// How long a push service keeps a signal for an unreachable device.
const TTL_SECS: u32 = 24 * 3600;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

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
        let public = encoded(&PublicKey::from(key.verifying_key()));
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
}

/// `key` as an uncompressed SEC1 point: `0x04 ‖ x ‖ y`.
fn encoded(key: &PublicKey) -> [u8; 65] {
    key.to_uncompressed_point().into()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

/// Randomised checks of the two parsers fed by anonymous clients: the
/// module is private to the crate, so an external fuzz target cannot reach
/// them. A fixed seed keeps failures reproducible.
#[cfg(test)]
mod properties {
    use rand::rngs::StdRng;
    use rand::{Rng as _, SeedableRng as _};

    use super::policy::KNOWN_SERVICES;
    use super::*;

    const PLAIN: &[&str] = &[
        "https://",
        "http://",
        "fcm.googleapis.com",
        "notify.windows.com",
        "ntfy.sh",
        "web.push.apple.com",
        "evil.com",
        ".",
        ":",
        "443",
        "8443",
        "@",
        "/",
        "::1",
        "127.0.0.1",
        "%2e",
        "FCM",
        "a",
        "-",
        ":443",
        "xn--",
    ];
    /// NUL, tab, LF, CR, space, number sign, star, question mark, brackets,
    /// backslash, e acute, line separator.
    const CODES: &[u32] = &[0, 9, 10, 13, 32, 35, 42, 63, 91, 93, 92, 0xe9, 0x2028];

    fn pieces() -> Vec<String> {
        PLAIN
            .iter()
            .map(ToString::to_string)
            .chain(
                CODES
                    .iter()
                    .filter_map(|&c| char::from_u32(c))
                    .map(String::from),
            )
            .collect()
    }

    fn endpoint(rng: &mut StdRng, pieces: &[String]) -> String {
        (0..rng.gen_range(1..10))
            .map(|_| {
                pieces
                    .get(rng.gen_range(0..pieces.len()))
                    .map_or("", String::as_str)
            })
            .collect()
    }

    #[test]
    fn subscriptions_parse_only_what_they_wrote() {
        let pieces = pieces();
        let mut rng = StdRng::seed_from_u64(0x5eed);
        for _ in 0..200_000 {
            let mut raw = vec![0u8; rng.gen_range(0..120)];
            rng.fill(raw.as_mut_slice());
            if rng.gen_bool(0.5) {
                let tail = endpoint(&mut rng, &pieces);
                raw.truncate(81);
                raw.extend_from_slice(tail.as_bytes());
            }
            match Subscription::from_bytes(&raw) {
                Some(sub) => assert_eq!(sub.to_bytes(), raw),
                None => assert!(
                    raw.len() < 81
                        || raw
                            .get(81..)
                            .is_some_and(|e| std::str::from_utf8(e).is_err())
                ),
            }
        }
    }

    /// The services the policy below allows, as the spec says: exact
    /// names, or any subdomain of a `*.` entry.
    fn known(host: &str) -> bool {
        KNOWN_SERVICES
            .iter()
            .chain(&["push.example.org"])
            .any(|known| match known.strip_prefix("*.") {
                Some(domain) => host.ends_with(&format!(".{domain}")),
                None => host == *known,
            })
    }

    /// Letters, digits and hyphens in labels that are not empty.
    fn plain_dns_name(host: &str) -> bool {
        host.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
    }

    /// An endpoint that starts like a real one half the time, with random
    /// pieces around and inside the host.
    fn likely_endpoint(rng: &mut StdRng, pieces: &[String]) -> String {
        if rng.gen_bool(0.5) {
            return endpoint(rng, pieces);
        }
        let hosts = ["fcm.googleapis.com", "a.notify.windows.com", "ntfy.sh"];
        let host = hosts.get(rng.gen_range(0..hosts.len())).map_or("", |h| *h);
        let around = |rng: &mut StdRng| {
            if rng.gen_bool(0.5) {
                String::new()
            } else {
                endpoint(rng, pieces)
            }
        };
        let (before, after) = (around(rng), around(rng));
        format!("https://{before}{host}{after}")
    }

    /// Whatever an endpoint holds, one that passes names a known push
    /// service by a plain DNS name, over HTTPS on port 443; and endpoints
    /// that are fine do pass.
    #[test]
    fn checked_endpoints_name_a_known_service_by_dns_name() {
        let pieces = pieces();
        let policy = Policy::new(&["push.example.org".into()]);
        let mut rng = StdRng::seed_from_u64(0xe0d);
        let (mut passed, mut bad) = (0, Vec::new());
        for _ in 0..200_000 {
            let e = likely_endpoint(&mut rng, &pieces);
            let Some(t) = policy.check(&e) else { continue };
            passed += 1;
            let sound = t.tls
                && t.port == 443
                && plain_dns_name(&t.host)
                && known(&t.host)
                && !t.path.chars().any(|c| c.is_ascii_control() || c == ' ');
            if !sound {
                bad.push(e);
            }
        }
        bad.sort();
        bad.dedup();
        bad.truncate(8);
        assert_eq!(bad, Vec::<String>::new());
        assert!(passed > 1000, "only {passed} endpoints passed");
    }
}
