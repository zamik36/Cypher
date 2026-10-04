//! Connecting to a host name the way browsers do (RFC 8305, simplified):
//! the resolved addresses are tried in an order that alternates IPv6 and
//! IPv4, a new attempt starts every [`ATTEMPT_DELAY`] while earlier ones are
//! still pending (or at once when one fails), and the first to connect wins.
//! A dead path, such as `::1` when the server listens on IPv4 only, then
//! costs a quarter second instead of the operating system's full retry.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::{TcpStream, lookup_host};
use tokio::task::JoinSet;

/// How long an attempt runs alone before the next address is tried too.
const ATTEMPT_DELAY: Duration = Duration::from_millis(250);

/// Resolves `host` and connects to the first address that answers. Losing
/// attempts are cancelled; the caller bounds the whole call with a timeout.
pub(crate) async fn dial(host: &str, port: u16) -> io::Result<TcpStream> {
    let mut queue = interleave(lookup_host((host, port)).await?.collect()).into_iter();
    let mut attempts = JoinSet::new();
    let mut last_error = None;
    loop {
        if let Some(addr) = queue.next() {
            attempts.spawn(TcpStream::connect(addr));
        }
        let more_to_try = queue.len() > 0;
        tokio::select! {
            joined = attempts.join_next() => match joined {
                Some(Ok(Ok(stream))) => return Ok(stream),
                Some(Ok(Err(e))) => last_error = Some(e),
                Some(Err(e)) => last_error = Some(io::Error::other(e)),
                None if !more_to_try => {
                    return Err(last_error.unwrap_or_else(|| {
                        io::Error::new(io::ErrorKind::NotFound, format!("{host} has no address"))
                    }));
                }
                None => {}
            },
            () = tokio::time::sleep(ATTEMPT_DELAY), if more_to_try => {}
        }
    }
}

/// Alternates address families, starting with the family of the first
/// resolved address, and otherwise keeps the resolver's order.
fn interleave(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let first_is_v6 = addrs.first().is_some_and(SocketAddr::is_ipv6);
    let (mut preferred, mut other): (Vec<_>, Vec<_>) =
        addrs.into_iter().partition(|a| a.is_ipv6() == first_is_v6);
    let mut out = Vec::with_capacity(preferred.len() + other.len());
    preferred.reverse();
    other.reverse();
    loop {
        match (preferred.pop(), other.pop()) {
            (None, None) => return out,
            (a, b) => out.extend(a.into_iter().chain(b)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::time::Instant;

    use tokio::net::TcpListener;

    use super::*;

    fn v4(last: u8) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::new(10, 0, 0, last), 1))
    }

    fn v6(last: u16) -> SocketAddr {
        SocketAddr::from((Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, last), 1))
    }

    #[test]
    fn families_alternate_from_the_resolvers_first_choice() {
        assert_eq!(
            interleave(vec![v6(1), v6(2), v6(3), v4(1)]),
            [v6(1), v4(1), v6(2), v6(3)]
        );
        assert_eq!(interleave(vec![v4(1), v4(2), v6(1)]), [v4(1), v6(1), v4(2)]);
        assert!(interleave(Vec::new()).is_empty());
    }

    #[tokio::test]
    async fn a_dead_address_does_not_delay_a_live_one() {
        // `localhost` may resolve to ::1 first, where nothing listens.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let started = Instant::now();
        dial("localhost", port).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn every_address_failing_reports_the_last_error() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        dial("127.0.0.1", port).await.unwrap_err();
    }
}
