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

/// The networks this deployment's reverse proxy connects from: only their
/// WebSocket connections may name the client in `X-Forwarded-For`, since
/// anyone else could put any address there. Trusts no one by default.
///
/// Parsed from a comma-separated list of addresses and CIDR networks, e.g.
/// `"172.30.0.2, fd00::/8"`.
#[cfg(feature = "ws")]
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies(Arc<[Network]>);

/// An IP network: the address with all but its first `prefix` bits ignored.
#[cfg(feature = "ws")]
#[derive(Debug, Clone, Copy)]
struct Network {
    addr: IpAddr,
    prefix: u8,
}

#[cfg(feature = "ws")]
impl TrustedProxies {
    /// Loopback and the private ranges (RFC 1918, IPv6 unique local): any
    /// host on such a network, not only the proxy, is trusted.
    pub fn private_networks() -> Self {
        use std::net::Ipv4Addr;
        let v4 = |a, b, prefix| Network {
            addr: IpAddr::V4(Ipv4Addr::new(a, b, 0, 0)),
            prefix,
        };
        let v6 = |segment, prefix| Network {
            addr: IpAddr::V6(Ipv6Addr::new(segment, 0, 0, 0, 0, 0, 0, 0)),
            prefix,
        };
        Self(Arc::new([
            v4(127, 0, 8),
            v4(10, 0, 8),
            v4(172, 16, 12),
            v4(192, 168, 16),
            Network {
                addr: IpAddr::V6(Ipv6Addr::LOCALHOST),
                prefix: 128,
            },
            v6(0xfc00, 7),
        ]))
    }

    /// Whether a connection from `peer` comes from a trusted proxy.
    pub fn contains(&self, peer: IpAddr) -> bool {
        let peer = peer.to_canonical();
        self.0.iter().any(|net| net.contains(peer))
    }
}

#[cfg(feature = "ws")]
impl std::str::FromStr for TrustedProxies {
    type Err = cypher_types::Error;

    fn from_str(list: &str) -> Result<Self, Self::Err> {
        list.split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(Network::parse)
            .collect::<Result<Vec<_>, _>>()
            .map(|nets| Self(nets.into()))
    }
}

#[cfg(feature = "ws")]
impl Network {
    fn parse(entry: &str) -> Result<Self, cypher_types::Error> {
        let invalid = || cypher_types::Error::Config(format!("invalid proxy network {entry}"));
        let (addr, prefix) = match entry.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix.parse::<u8>().map_err(|_| invalid())?)),
            None => (entry, None),
        };
        let addr = addr
            .parse::<IpAddr>()
            .map_err(|_| invalid())?
            .to_canonical();
        let bits = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(bits);
        if prefix > bits {
            return Err(invalid());
        }
        Ok(Self { addr, prefix })
    }

    /// `ip` must be canonical (IPv4 not mapped into IPv6).
    fn contains(self, ip: IpAddr) -> bool {
        let prefix = u32::from(self.prefix);
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            (IpAddr::V4(_), IpAddr::V6(_)) | (IpAddr::V6(_), IpAddr::V4(_)) => false,
        }
    }
}

/// The client address a trusted proxy put in `X-Forwarded-For`: the last
/// entry, the one our proxy appended (earlier ones come from the client).
#[cfg(feature = "ws")]
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
    #[cfg(feature = "ws")]
    fn no_proxy_is_trusted_unless_named() {
        let none = TrustedProxies::default();
        for peer in ["127.0.0.1", "10.1.2.3", "172.18.0.9", "::1", "fd00::1"] {
            assert!(!none.contains(ip(peer)), "{peer}");
        }
        assert!(
            !"".parse::<TrustedProxies>()
                .unwrap()
                .contains(ip("127.0.0.1"))
        );
    }

    #[test]
    #[cfg(feature = "ws")]
    fn named_proxies_are_trusted_and_nothing_else() {
        let named: TrustedProxies = " 172.30.0.2 , 2001:db8::/32,10.0.0.0/8".parse().unwrap();
        for trusted in [
            "172.30.0.2",
            "::ffff:172.30.0.2",
            "2001:db8:5::1",
            "10.200.0.1",
        ] {
            assert!(named.contains(ip(trusted)), "{trusted}");
        }
        for other in ["172.30.0.3", "2001:db9::1", "11.0.0.1", "::1"] {
            assert!(!named.contains(ip(other)), "{other}");
        }
        let everyone: TrustedProxies = "0.0.0.0/0".parse().unwrap();
        assert!(everyone.contains(ip("203.0.113.1")));
        assert!(
            !everyone.contains(ip("2001:db8::1")),
            "IPv4 networks hold no IPv6"
        );
        for invalid in [
            "10.0.0.0/33",
            "::/129",
            "garbage",
            "10.0.0.1/x",
            "10.0.0.1/",
        ] {
            assert!(invalid.parse::<TrustedProxies>().is_err(), "{invalid}");
        }
    }

    #[test]
    #[cfg(feature = "ws")]
    fn private_networks_trust_loopback_and_private_ranges() {
        let private = TrustedProxies::private_networks();
        for trusted in [
            "127.0.0.1",
            "10.1.2.3",
            "172.18.0.5",
            "192.168.1.1",
            "::1",
            "fd00::1",
        ] {
            assert!(private.contains(ip(trusted)), "{trusted}");
        }
        for public in ["203.0.113.1", "172.32.0.1", "2001:db8::1"] {
            assert!(!private.contains(ip(public)), "{public}");
        }
    }

    #[test]
    #[cfg(feature = "ws")]
    fn the_proxy_names_the_client_last() {
        assert_eq!(
            forwarded_client("1.2.3.4, 203.0.113.9"),
            Some(ip("203.0.113.9"))
        );
        assert_eq!(forwarded_client(" 2001:db8::5 "), Some(ip("2001:db8::5")));
        assert_eq!(forwarded_client("garbage"), None);
    }
}
