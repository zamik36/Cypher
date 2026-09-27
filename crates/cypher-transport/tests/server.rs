#![expect(
    clippy::unwrap_used,
    reason = "test helpers fail loudly on broken fixtures"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use cypher_transport::server::{Handler, Listener, Upgrade, serve};
use cypher_transport::{ClientConn, FrameSink, FrameStream, connect_tls};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

/// Echoes frames back and counts refused connections.
#[derive(Default)]
struct Echo {
    rejected: AtomicUsize,
}

impl Handler for Echo {
    async fn serve(self: Arc<Self>, mut stream: FrameStream, mut sink: FrameSink) {
        while let Some(Ok(frame)) = stream.next().await {
            if sink.send(frame).await.is_err() {
                break;
            }
        }
    }

    fn rejected(&self) {
        self.rejected.fetch_add(1, Ordering::SeqCst);
    }
}

struct Server {
    addr: String,
    client: Arc<rustls::ClientConfig>,
    echo: Arc<Echo>,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

async fn start(max_connections: usize) -> Server {
    let cert = cypher_tls::SelfSignedCert::generate(&["localhost"]).unwrap();
    let client = cypher_tls::make_client_config_with_pem(&cert.cert_pem).unwrap();
    let acceptor = TlsAcceptor::from(cypher_tls::make_server_config_from_cert(cert).unwrap());
    let listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Upgrade::Tls(acceptor))
        .await
        .unwrap();
    let addr = format!("localhost:{}", listener.local_addr().unwrap().port());
    let (echo, shutdown) = (Arc::new(Echo::default()), CancellationToken::new());
    let task = tokio::spawn(serve(
        vec![listener],
        Arc::clone(&echo),
        max_connections,
        shutdown.clone(),
    ));
    Server {
        addr,
        client,
        echo,
        shutdown,
        task,
    }
}

async fn echo_roundtrip(conn: &mut ClientConn, payload: &'static [u8]) -> bool {
    if conn.send(Bytes::from_static(payload)).await.is_err() {
        return false;
    }
    matches!(
        tokio::time::timeout(Duration::from_secs(2), conn.next()).await,
        Ok(Some(Ok(frame))) if frame.as_ref() == payload
    )
}

#[tokio::test]
async fn connections_beyond_the_limit_are_refused_and_counted() {
    let server = start(1).await;
    let mut first = connect_tls(&server.addr, Arc::clone(&server.client))
        .await
        .unwrap();
    assert!(echo_roundtrip(&mut first, b"one").await);

    let refused = match connect_tls(&server.addr, Arc::clone(&server.client)).await {
        Ok(mut conn) => !echo_roundtrip(&mut conn, b"two").await,
        Err(_) => true,
    };
    assert!(refused, "second connection must not be served");
    assert_eq!(server.echo.rejected.load(Ordering::SeqCst), 1);

    drop(first);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut third = connect_tls(&server.addr, Arc::clone(&server.client))
        .await
        .unwrap();
    assert!(
        echo_roundtrip(&mut third, b"three").await,
        "permit is released on close"
    );
}

#[tokio::test]
async fn a_stalled_handshake_does_not_block_other_clients() {
    let server = start(8).await;
    let port = server.addr.rsplit_once(':').unwrap().1;
    let _silent = TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    let mut conn = connect_tls(&server.addr, Arc::clone(&server.client))
        .await
        .unwrap();
    assert!(echo_roundtrip(&mut conn, b"still serving").await);
}

#[tokio::test]
async fn shutdown_stops_accepting_and_closes_open_connections() {
    let server = start(8).await;
    let mut conn = connect_tls(&server.addr, Arc::clone(&server.client))
        .await
        .unwrap();
    assert!(echo_roundtrip(&mut conn, b"before").await);

    server.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(2), server.task)
        .await
        .expect("serve returns after shutdown")
        .unwrap();
    assert!(!echo_roundtrip(&mut conn, b"after").await);
    connect_tls(&server.addr, Arc::clone(&server.client))
        .await
        .unwrap_err();
}
