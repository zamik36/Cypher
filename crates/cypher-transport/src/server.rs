//! Accept loop shared by every service that terminates client connections.
//!
//! Each accepted connection takes a permit and a slot for its client address
//! (excess connections are closed at once), is upgraded on its own task so
//! a slow handshake never blocks accepting, and is dropped when the service
//! shuts down. A client connecting directly is counted before its handshake;
//! one behind our reverse proxy only once the upgrade request names it.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use crate::limits::{ConnectionLimits, IpSlot, IpSlots};
use crate::{FrameSink, FrameStream, Result, accept, accept_tls, split};

/// Serves upgraded frame connections.
pub trait Handler: Send + Sync + 'static {
    /// Drives one connection until it closes.
    fn serve(
        self: Arc<Self>,
        stream: FrameStream,
        sink: FrameSink,
    ) -> impl Future<Output = ()> + Send;

    /// A connection was refused because every permit was taken.
    fn rejected(&self) {}
}

/// How an accepted TCP connection becomes a frame connection.
#[derive(Clone)]
pub enum Upgrade {
    Tls(TlsAcceptor),
    /// Plain WebSocket; TLS is terminated by a reverse proxy in front.
    #[cfg(feature = "ws")]
    WebSocket,
}

/// An upgraded connection and, behind our proxy, the client it forwards.
type Upgraded = (FrameStream, FrameSink, Option<IpAddr>);

impl Upgrade {
    async fn apply(&self, tcp: TcpStream) -> Result<Upgraded> {
        match self {
            Self::Tls(acceptor) => accept_tls(acceptor, tcp).await.map(|conn| {
                let (stream, sink) = split(conn);
                (stream, sink, None)
            }),
            #[cfg(feature = "ws")]
            Self::WebSocket => crate::ws::accept_ws(tcp).await,
        }
    }

    /// Whether a connection from `peer` may name another client: only a
    /// WebSocket from our own reverse proxy does.
    fn forwards_for(&self, peer: IpAddr) -> bool {
        match self {
            Self::Tls(_) => false,
            #[cfg(feature = "ws")]
            Self::WebSocket => crate::limits::is_trusted_proxy(peer),
        }
    }
}

/// A bound listener and how its connections are upgraded.
pub struct Listener {
    listener: TcpListener,
    upgrade: Upgrade,
}

impl Listener {
    pub fn new(listener: TcpListener, upgrade: Upgrade) -> Self {
        Self { listener, upgrade }
    }

    pub async fn bind(addr: SocketAddr, upgrade: Upgrade) -> std::io::Result<Self> {
        Ok(Self::new(TcpListener::bind(addr).await?, upgrade))
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

/// Accepts on every listener until `shutdown`. All listeners share the
/// limits.
pub async fn serve<H: Handler>(
    listeners: Vec<Listener>,
    handler: Arc<H>,
    limits: ConnectionLimits,
    shutdown: CancellationToken,
) {
    let shared = Shared {
        permits: Arc::new(Semaphore::new(limits.total)),
        slots: IpSlots::new(limits.per_ip),
    };
    let loops = listeners.into_iter().map(|listener| {
        accept_loop(
            listener,
            Arc::clone(&handler),
            shared.clone(),
            shutdown.clone(),
        )
    });
    futures::future::join_all(loops).await;
}

#[derive(Clone)]
struct Shared {
    permits: Arc<Semaphore>,
    slots: Arc<IpSlots>,
}

/// A client's address slot: taken (or refused), or still to be taken once
/// the proxy names the client.
enum Admission {
    Admitted(Option<IpSlot>),
    Deferred,
    Refused,
}

fn admit_early(upgrade: &Upgrade, slots: &Arc<IpSlots>, peer: Option<IpAddr>) -> Admission {
    match peer {
        Some(ip) if upgrade.forwards_for(ip) => Admission::Deferred,
        Some(ip) => slots
            .acquire(ip)
            .map_or(Admission::Refused, |slot| Admission::Admitted(Some(slot))),
        None => Admission::Admitted(None),
    }
}

/// The admission once the upgrade named the client, if it was deferred.
fn admit_late(early: Admission, client: Option<IpAddr>, slots: &Arc<IpSlots>) -> Admission {
    match (early, client) {
        (Admission::Deferred, Some(ip)) => slots
            .acquire(ip)
            .map_or(Admission::Refused, |slot| Admission::Admitted(Some(slot))),
        (Admission::Deferred, None) => Admission::Admitted(None),
        (decided, _) => decided,
    }
}

async fn accept_loop<H: Handler>(
    Listener { listener, upgrade }: Listener,
    handler: Arc<H>,
    Shared { permits, slots }: Shared,
    shutdown: CancellationToken,
) {
    loop {
        let tcp = tokio::select! {
            () = shutdown.cancelled() => return,
            tcp = accept(&listener) => tcp,
        };
        let peer = tcp.peer_addr().ok().map(|a| a.ip());
        let early = admit_early(&upgrade, &slots, peer);
        if matches!(early, Admission::Refused) {
            handler.rejected();
            continue;
        }
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            handler.rejected();
            continue;
        };
        // A child token per connection: polling one shared token from every
        // connection task serialises all workers on its waiter lock.
        let (upgrade, handler, slots, shutdown) = (
            upgrade.clone(),
            Arc::clone(&handler),
            Arc::clone(&slots),
            shutdown.child_token(),
        );
        tokio::spawn(async move {
            let _permit = permit;
            let connection = async {
                let Ok((stream, sink, forwarded)) = upgrade.apply(tcp).await else {
                    return;
                };
                let Admission::Admitted(_slot) = admit_late(early, forwarded.or(peer), &slots)
                else {
                    handler.rejected();
                    return;
                };
                handler.serve(stream, sink).await;
            };
            tokio::select! {
                () = shutdown.cancelled() => {}
                () = connection => {}
            }
        });
    }
}
