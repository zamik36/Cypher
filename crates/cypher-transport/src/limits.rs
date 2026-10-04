//! Connection limits per client address, so one host cannot take every
//! connection slot of a service.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex, PoisonError};

/// How many connections a service accepts, in total and from one address.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionLimits {
    pub total: usize,
    /// Generous on purpose: mobile carriers put many users behind one
    /// address (CGNAT).
    pub per_ip: usize,
}

/// Open connections per client, counted by IPv4 address or IPv6 /64: one
/// host is usually given a whole /64.
#[derive(Debug)]
pub(crate) struct IpSlots {
    per_ip: usize,
    open: Mutex<HashMap<IpAddr, usize>>,
}

/// A held connection slot; released when dropped.
#[derive(Debug)]
pub(crate) struct IpSlot {
    slots: Arc<IpSlots>,
    key: IpAddr,
}

impl IpSlots {
    pub(crate) fn new(per_ip: usize) -> Arc<Self> {
        Arc::new(Self {
            per_ip,
            open: Mutex::new(HashMap::new()),
        })
    }

    /// Takes a slot for `ip`, or `None` when it already holds `per_ip`.
    pub(crate) fn acquire(self: &Arc<Self>, ip: IpAddr) -> Option<IpSlot> {
        let key = client_key(ip);
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let count = open.entry(key).or_default();
        if *count >= self.per_ip {
            return None;
        }
        *count += 1;
        Some(IpSlot {
            slots: Arc::clone(self),
            key,
        })
    }
}

impl Drop for IpSlot {
    fn drop(&mut self) {
        let mut open = self
            .slots
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = open.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                open.remove(&self.key);
            }
        }
    }
}

/// The address a client is counted under: IPv4 as is (also when mapped
/// into IPv6), IPv6 by its /64 network.
fn client_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let mut segments = v6.segments();
            for s in segments.iter_mut().skip(4) {
                *s = 0;
            }
            IpAddr::V6(Ipv6Addr::from(segments))
        }
        v4 @ IpAddr::V4(_) => v4,
    }
}

/// Whether `peer` is this deployment's own reverse proxy (loopback or a
/// private network such as Docker's), whose forwarded client address can
/// be believed. Anyone else could put any address in the header.
pub(crate) fn is_trusted_proxy(peer: IpAddr) -> bool {
    match peer.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
    }
}

/// The client address a trusted proxy put in `X-Forwarded-For`: the last
/// entry, the one our proxy appended (earlier ones come from the client).
pub(crate) fn forwarded_client(header: &str) -> Option<IpAddr> {
    header.rsplit(',').next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn an_address_holds_at_most_its_share_until_it_releases_one() {
        let slots = IpSlots::new(2);
        let a = slots.acquire(ip("203.0.113.1")).unwrap();
        let _b = slots.acquire(ip("203.0.113.1")).unwrap();
        assert!(slots.acquire(ip("203.0.113.1")).is_none());
        assert!(
            slots.acquire(ip("203.0.113.2")).is_some(),
            "others are unaffected"
        );
        drop(a);
        assert!(slots.acquire(ip("203.0.113.1")).is_some());
    }

    #[test]
    fn ipv6_counts_per_64_and_mapped_ipv4_as_ipv4() {
        let slots = IpSlots::new(1);
        let _a = slots.acquire(ip("2001:db8:1:2::1")).unwrap();
        assert!(
            slots.acquire(ip("2001:db8:1:2::ffff")).is_none(),
            "same /64"
        );
        assert!(
            slots.acquire(ip("2001:db8:1:3::1")).is_some(),
            "another /64"
        );

        let _v4 = slots
            .acquire(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)))
            .unwrap();
        assert!(slots.acquire(ip("::ffff:198.51.100.7")).is_none());
    }

    #[test]
    fn only_the_deployments_own_proxy_is_trusted_with_client_addresses() {
        for trusted in [
            "127.0.0.1",
            "10.1.2.3",
            "172.18.0.5",
            "192.168.1.1",
            "::1",
            "fd00::1",
        ] {
            assert!(is_trusted_proxy(ip(trusted)), "{trusted}");
        }
        for public in ["203.0.113.1", "2001:db8::1"] {
            assert!(!is_trusted_proxy(ip(public)), "{public}");
        }
        assert_eq!(
            forwarded_client("1.2.3.4, 203.0.113.9"),
            Some(ip("203.0.113.9"))
        );
        assert_eq!(forwarded_client(" 2001:db8::5 "), Some(ip("2001:db8::5")));
        assert_eq!(forwarded_client("garbage"), None);
    }
}
