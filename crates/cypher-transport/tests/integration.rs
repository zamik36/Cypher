//! Integration tests for the transport layer.
//!
//! These tests spin up real TLS server+client pairs in-process using
//! tokio tasks and verify the full frame encode/decode/session handshake flow.

use std::sync::Arc;

use bytes::Bytes;
use cypher_tls::config::make_client_config_with_cert;
use cypher_transport::frame::FrameFlags;
use cypher_transport::server::TransportListener;
use cypher_transport::session::TransportSession;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Install the `ring` CryptoProvider as the process default.
///
/// rustls 0.23 requires an explicit provider when multiple backends are
/// available through the dependency graph (e.g. `ring` + `aws-lc-rs` from
/// `rcgen`). Calling this at the top of every test is safe and idempotent.
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Generate a self-signed cert, build matching server + client TLS configs, and
/// return them together with the cert's DER bytes.
fn make_tls_pair() -> (Arc<rustls::ServerConfig>, Arc<rustls::ClientConfig>) {
    install_crypto_provider();
    let cert = cypher_tls::SelfSignedCert::generate(&["localhost"]).unwrap();
    let cert_der = cert.cert_der.clone();
    let server_config = cypher_tls::make_server_config_from_cert(cert).unwrap();
    let client_config = make_client_config_with_cert(cert_der).unwrap();
    (server_config, client_config)
}

/// Bind a `TransportListener` on a random port and return (listener, port).
///
/// The listener binds on `127.0.0.1:0`; clients must connect via
/// `localhost:<port>` so that the TLS SNI hostname matches the cert's DNS SAN.
async fn bind_listener(server_config: Arc<rustls::ServerConfig>) -> (TransportListener, u16) {
    // Use a plain TcpListener first to obtain a free ephemeral port, then
    // release it and let TransportListener re-bind to the same address.
    // There is a tiny TOCTOU window here, but it is acceptable in tests.
    let tmp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tmp.local_addr().unwrap().port();
    drop(tmp);

    let addr = client_addr(port);
    let listener = TransportListener::bind(&addr, server_config).await.unwrap();
    (listener, port)
}

/// Return the connect address for the client.
///
/// We always connect via the `localhost` hostname so that TLS can match the
/// DNS SAN on the self-signed certificate.
fn client_addr(port: u16) -> String {
    format!("localhost:{port}")
}

// ---------------------------------------------------------------------------
// Test 1 – basic bidirectional frame exchange
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_client_server_frame_exchange() {
    let (server_config, client_config) = make_tls_pair();
    let (mut listener, port) = bind_listener(server_config).await;

    let server_task = tokio::spawn(async move {
        let mut session = listener.accept().await.unwrap();

        // Send a frame to the client.
        session
            .send_frame(Bytes::from_static(b"hello from server"), FrameFlags::NONE)
            .await
            .unwrap();

        // Receive the client's reply.
        let frame = session.recv_frame().await.unwrap();
        frame.payload
    });

    // Client connects and does the mirror-image.
    let addr = client_addr(port);
    let mut client = TransportSession::connect(&addr, client_config)
        .await
        .unwrap();

    // Receive the server's greeting.
    let server_frame = client.recv_frame().await.unwrap();
    assert_eq!(
        server_frame.payload,
        Bytes::from_static(b"hello from server")
    );

    // Send a reply.
    client
        .send_frame(Bytes::from_static(b"hello from client"), FrameFlags::NONE)
        .await
        .unwrap();

    // Verify the server received the correct payload.
    let client_payload = server_task.await.unwrap();
    assert_eq!(client_payload, Bytes::from_static(b"hello from client"));
}

// ---------------------------------------------------------------------------
// Test 2 – PING / PONG exchange
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_ping_pong() {
    let (server_config, client_config) = make_tls_pair();
    let (mut listener, port) = bind_listener(server_config).await;

    let server_task = tokio::spawn(async move {
        let mut session = listener.accept().await.unwrap();

        // Expect a PING from the client.
        let frame = session.recv_frame().await.unwrap();
        assert!(
            frame.flags.contains(FrameFlags::PING),
            "expected PING flag, got flags={:?}",
            frame.flags
        );

        // Reply with a PONG.
        session.send_pong().await.unwrap();
    });

    let addr = client_addr(port);
    let mut client = TransportSession::connect(&addr, client_config)
        .await
        .unwrap();

    // Send PING.
    client.send_ping().await.unwrap();

    // Receive PONG.
    let pong = client.recv_frame().await.unwrap();
    assert!(
        pong.flags.contains(FrameFlags::PONG),
        "expected PONG flag, got flags={:?}",
        pong.flags
    );

    server_task.await.unwrap();
}

// ---------------------------------------------------------------------------
// Test 3 – graceful session close
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_session_close() {
    let (server_config, client_config) = make_tls_pair();
    let (mut listener, port) = bind_listener(server_config).await;

    let server_task = tokio::spawn(async move {
        let mut session = listener.accept().await.unwrap();

        // The first frame should be SESSION_CLOSE.
        let frame = session.recv_frame().await.unwrap();
        assert!(
            frame.flags.contains(FrameFlags::SESSION_CLOSE),
            "expected SESSION_CLOSE flag, got flags={:?}",
            frame.flags
        );

        // After the peer closes, any further read must return ConnectionClosed.
        let result = session.recv_frame().await;
        assert!(
            matches!(result, Err(cypher_common::Error::ConnectionClosed)),
            "expected ConnectionClosed, got: {result:?}"
        );
    });

    let addr = client_addr(port);
    let mut client = TransportSession::connect(&addr, client_config)
        .await
        .unwrap();

    // Gracefully close from the client side.
    client.close().await.unwrap();

    server_task.await.unwrap();
}

// ---------------------------------------------------------------------------
// Test 4 – large payload (512 KiB)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_large_payload() {
    const SIZE: usize = 512 * 1024; // 512 KiB

    let (server_config, client_config) = make_tls_pair();
    let (mut listener, port) = bind_listener(server_config).await;

    let server_task = tokio::spawn(async move {
        let mut session = listener.accept().await.unwrap();
        let frame = session.recv_frame().await.unwrap();
        frame.payload
    });

    let addr = client_addr(port);
    let mut client = TransportSession::connect(&addr, client_config)
        .await
        .unwrap();

    let large_data: Bytes = Bytes::from(vec![0xABu8; SIZE]);
    client
        .send_frame(large_data.clone(), FrameFlags::NONE)
        .await
        .unwrap();

    let received = server_task.await.unwrap();
    assert_eq!(received.len(), SIZE, "payload length mismatch");
    assert_eq!(received, large_data, "payload content mismatch");
}

// ---------------------------------------------------------------------------
// Test 5 – multiple frames in order
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_multiple_frames() {
    const FRAME_COUNT: u32 = 100;

    let (server_config, client_config) = make_tls_pair();
    let (mut listener, port) = bind_listener(server_config).await;

    let server_task = tokio::spawn(async move {
        let mut session = listener.accept().await.unwrap();
        let mut payloads = Vec::with_capacity(FRAME_COUNT as usize);
        for _ in 0..FRAME_COUNT {
            let frame = session.recv_frame().await.unwrap();
            payloads.push(frame.payload);
        }
        payloads
    });

    let addr = client_addr(port);
    let mut client = TransportSession::connect(&addr, client_config)
        .await
        .unwrap();

    for i in 0u32..FRAME_COUNT {
        // Encode the frame index as a 4-byte big-endian payload so we can
        // verify both order and content on the server side.
        let payload = Bytes::copy_from_slice(&i.to_be_bytes());
        client.send_frame(payload, FrameFlags::NONE).await.unwrap();
    }

    let received = server_task.await.unwrap();

    assert_eq!(
        received.len(),
        FRAME_COUNT as usize,
        "received wrong number of frames"
    );

    for (i, payload) in received.iter().enumerate() {
        let expected = (i as u32).to_be_bytes();
        assert_eq!(
            payload.as_ref(),
            &expected,
            "frame {i} payload mismatch: expected {expected:?}, got {payload:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 6 – sequence numbers increment correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_sequence_numbers() {
    let (server_config, client_config) = make_tls_pair();
    let (mut listener, port) = bind_listener(server_config).await;

    let server_task = tokio::spawn(async move {
        let mut session = listener.accept().await.unwrap();
        let mut seq_nos = Vec::new();
        for _ in 0..3 {
            let frame = session.recv_frame().await.unwrap();
            seq_nos.push(frame.seq_no);
        }
        seq_nos
    });

    let addr = client_addr(port);
    let mut client = TransportSession::connect(&addr, client_config)
        .await
        .unwrap();

    for _ in 0..3 {
        client
            .send_frame(Bytes::from_static(b"seq-test"), FrameFlags::NONE)
            .await
            .unwrap();
    }

    let seq_nos = server_task.await.unwrap();

    assert_eq!(
        seq_nos,
        vec![1, 2, 3],
        "sequence numbers should start at 1 and increment by 1"
    );
}

// ---------------------------------------------------------------------------
// Test 7 – ack field mirrors the last received sequence number
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_ack_field() {
    let (server_config, client_config) = make_tls_pair();
    let (mut listener, port) = bind_listener(server_config).await;

    let server_task = tokio::spawn(async move {
        let mut session = listener.accept().await.unwrap();

        // Receive two frames from the client.
        let _f1 = session.recv_frame().await.unwrap();
        let _f2 = session.recv_frame().await.unwrap();

        // Now send one frame back; its ack field should reflect seq_no 2.
        session
            .send_frame(Bytes::from_static(b"ack-reply"), FrameFlags::NONE)
            .await
            .unwrap();
    });

    let addr = client_addr(port);
    let mut client = TransportSession::connect(&addr, client_config)
        .await
        .unwrap();

    client
        .send_frame(Bytes::from_static(b"first"), FrameFlags::NONE)
        .await
        .unwrap();
    client
        .send_frame(Bytes::from_static(b"second"), FrameFlags::NONE)
        .await
        .unwrap();

    let reply = client.recv_frame().await.unwrap();

    // The server received frames 1 and 2, so its ack should be 2.
    assert_eq!(
        reply.ack, 2,
        "server ack should equal the last seq_no it received"
    );

    server_task.await.unwrap();
}
