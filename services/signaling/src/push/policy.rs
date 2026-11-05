use std::net::IpAddr;

use cypher_wire::MAX_PUSH_ENDPOINT_LEN as MAX_ENDPOINT_LEN;

/// Push services that endpoints may point at (`*.` matches subdomains).
pub(super) const KNOWN_SERVICES: &[&str] = &[
    "fcm.googleapis.com",
    "updates.push.services.mozilla.com",
    "*.notify.windows.com",
    "web.push.apple.com",
    "ntfy.sh",
];

/// Where endpoints may point, and how strictly.
#[derive(Debug, Clone)]
pub(crate) struct Policy {
    hosts: Vec<String>,
    /// Tests only: plain HTTP to loopback, any host.
    pub(super) insecure_loopback: bool,
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
            && dns_name(&host)
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

/// Whether `host` is a DNS name: labels of letters, digits and hyphens,
/// none empty. Anything else (brackets, control characters, `*`, a
/// trailing dot) could neither be resolved as meant nor go into the JWT
/// audience as is.
fn dns_name(host: &str) -> bool {
    !host.split('.').any(str::is_empty) && rustls_pki_types::DnsName::try_from(host).is_ok()
}

/// A checked endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(super) tls: bool,
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) path: String,
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

    pub(super) fn origin(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        match (self.tls, self.port) {
            (true, 443) | (false, 80) => format!("{scheme}://{}", self.host),
            _ => format!("{scheme}://{}:{}", self.host, self.port),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            // Not DNS names, though they end like an allowed one.
            "https://a\r\nx-injected.notify.windows.com/x",
            "https://a..notify.windows.com/x",
            "https://*.notify.windows.com/x",
            "https://a\u{2028}.notify.windows.com/x",
            "https://a\0.notify.windows.com/x",
            "https://a\".notify.windows.com/x",
            "https://fcm.googleapis.com./x",
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
}
