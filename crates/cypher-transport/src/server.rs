//! Accept loop shared by every service that terminates client connections.
//!
//! Each accepted connection takes a permit (excess connections are closed
//! at once), is upgraded on its own task so a slow handshake never blocks
//! accepting, and is dropped when the service shuts down.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

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

impl Upgrade {
    async fn apply(&self, tcp: TcpStream) -> Result<(FrameStream, FrameSink)> {
        match self {
            Self::Tls(acceptor) => accept_tls(acceptor, tcp).await.map(split),
            #[cfg(feature = "ws")]
            Self::WebSocket => crate::ws::accept_ws(tcp).await,
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

/// Accepts on every listener until `shutdown`. All listeners share
/// `max_connections` permits.
pub async fn serve<H: Handler>(
    listeners: Vec<Listener>,
    handler: Arc<H>,
    max_connections: usize,
    shutdown: CancellationToken,
) {
    let permits = Arc::new(Semaphore::new(max_connections));
    let loops = listeners.into_iter().map(|listener| {
        accept_loop(
            listener,
            Arc::clone(&handler),
            Arc::clone(&permits),
            shutdown.clone(),
        )
    });
    futures::future::join_all(loops).await;
}

async fn accept_loop<H: Handler>(
    Listener { listener, upgrade }: Listener,
    handler: Arc<H>,
    permits: Arc<Semaphore>,
    shutdown: CancellationToken,
) {
    loop {
        let tcp = tokio::select! {
            () = shutdown.cancelled() => return,
            tcp = accept(&listener) => tcp,
        };
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            handler.rejected();
            continue;
        };
        let (upgrade, handler, shutdown) =
            (upgrade.clone(), Arc::clone(&handler), shutdown.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let connection = async {
                if let Ok((stream, sink)) = upgrade.apply(tcp).await {
                    handler.serve(stream, sink).await;
                }
            };
            tokio::select! {
                () = shutdown.cancelled() => {}
                () = connection => {}
            }
        });
    }
}
