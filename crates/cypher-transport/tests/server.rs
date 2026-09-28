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

#[cfg(feature = "ws")]
mod websocket {
    use tokio::io::AsyncWriteExt;
    use tokio_tungstenite::tungstenite::Message;

    use super::*;

    async fn start_ws() -> (String, CancellationToken) {
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Upgrade::WebSocket)
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let shutdown = CancellationToken::new();
        tokio::spawn(serve(
            vec![listener],
            Arc::new(Echo::default()),
            8,
            shutdown.clone(),
        ));
        (addr, shutdown)
    }

    async fn next(
        ws: &mut (impl StreamExt<Item = Result<Message, impl std::fmt::Debug>> + Unpin),
    ) -> Option<Message> {
        tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .and_then(Result::ok)
    }

    #[tokio::test]
    async fn binary_messages_are_frames_and_others_are_skipped() {
        let (addr, _shutdown) = start_ws().await;
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        ws.send(Message::text("not a frame")).await.unwrap();
        ws.send(Message::binary(b"frame".to_vec())).await.unwrap();
        assert_eq!(
            next(&mut ws).await,
            Some(Message::binary(b"frame".to_vec()))
        );
    }

    #[tokio::test]
    async fn oversized_message_closes_the_connection() {
        let (addr, _shutdown) = start_ws().await;
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        let _ = ws
            .send(Message::binary(vec![0u8; cypher_types::MAX_FRAME_SIZE + 1]))
            .await;
        assert!(!matches!(next(&mut ws).await, Some(Message::Binary(_))));
    }

    #[tokio::test]
    async fn failed_handshake_leaves_other_clients_served() {
        let (addr, _shutdown) = start_ws().await;
        let mut raw = TcpStream::connect(&addr).await.unwrap();
        raw.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        ws.send(Message::binary(b"ok".to_vec())).await.unwrap();
        assert_eq!(next(&mut ws).await, Some(Message::binary(b"ok".to_vec())));
    }
}
