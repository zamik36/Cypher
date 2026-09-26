use std::sync::Arc;

use bytes::Bytes;
use cypher_transport::{accept_tls, connect_tls};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

async fn server() -> (u16, TlsAcceptor, Arc<rustls::ClientConfig>, TcpListener) {
    let cert = cypher_tls::SelfSignedCert::generate(&["localhost"]).unwrap();
    let client = cypher_tls::make_client_config_with_pem(&cert.cert_pem).unwrap();
    let acceptor = TlsAcceptor::from(cypher_tls::make_server_config_from_cert(cert).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (port, acceptor, client, listener)
}

#[tokio::test]
async fn frames_roundtrip_over_pinned_tls() {
    let (port, acceptor, client_cfg, listener) = server().await;
    let srv = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut conn = accept_tls(&acceptor, tcp).await.unwrap();
        while let Some(Ok(frame)) = conn.next().await {
            conn.send(frame.freeze()).await.unwrap();
        }
    });

    let mut conn = connect_tls(&format!("localhost:{port}"), client_cfg)
        .await
        .unwrap();
    let big = Bytes::from(vec![7u8; 900 * 1024]);
    for payload in [Bytes::from_static(b"hello"), Bytes::new(), big] {
        conn.send(payload.clone()).await.unwrap();
        assert_eq!(conn.next().await.unwrap().unwrap().freeze(), payload);
    }
    conn.close().await.unwrap();
    srv.await.unwrap();
}

#[tokio::test]
async fn oversized_frames_are_rejected() {
    let (port, acceptor, client_cfg, listener) = server().await;
    let srv = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut conn = accept_tls(&acceptor, tcp).await.unwrap();
        conn.next().await
    });
    let mut conn = connect_tls(&format!("localhost:{port}"), client_cfg)
        .await
        .unwrap();
    assert!(
        conn.send(Bytes::from(vec![0u8; 2 * 1024 * 1024]))
            .await
            .is_err()
    );
    drop(conn);
    let _ = srv.await;
}

#[tokio::test]
async fn untrusted_certificate_is_refused() {
    let (port, acceptor, _, listener) = server().await;
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let _ = accept_tls(&acceptor, tcp).await;
    });
    let other = cypher_tls::SelfSignedCert::generate(&["localhost"]).unwrap();
    let wrong_pin = cypher_tls::make_client_config_with_pem(&other.cert_pem).unwrap();
    assert!(
        connect_tls(&format!("localhost:{port}"), wrong_pin)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn silent_client_times_out_handshake() {
    let (_, acceptor, _, listener) = server().await;
    let addr = listener.local_addr().unwrap();
    let _idle = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (tcp, _) = listener.accept().await.unwrap();
    tokio::time::pause();
    let res = accept_tls(&acceptor, tcp).await;
    assert!(matches!(res, Err(cypher_types::Error::Timeout)));
}
