use std::io;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cypher_transport::server::Handler;
use cypher_transport::{FrameSink, FrameStream};
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};

use super::*;

/// Replies `ok:<request>`; `slow…` answers after a delay, `lost…` never.
struct FakeSignaling;

impl OnionUpstream for FakeSignaling {
    async fn forward(&self, request: Bytes) -> Option<Bytes> {
        if request.starts_with(b"lost") {
            return None;
        }
        if request.starts_with(b"slow") {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Some([b"ok:".as_slice(), &request].concat().into())
    }
}

struct Conn {
    tx: mpsc::Sender<io::Result<Bytes>>,
    rx: mpsc::Receiver<Bytes>,
}

impl Conn {
    async fn send(&mut self, corr: u64, body: &[u8]) {
        let frame = [corr.to_le_bytes().as_slice(), body].concat();
        self.tx.send(Ok(frame.into())).await.unwrap();
    }

    async fn send_raw(&mut self, frame: Vec<u8>) {
        self.tx.send(Ok(frame.into())).await.unwrap();
    }

    async fn reply(&mut self) -> (u64, Bytes) {
        let frame = tokio::time::timeout(Duration::from_secs(5), self.rx.next())
            .await
            .expect("reply in time")
            .expect("connection open");
        let corr = u64::from_le_bytes(frame[..CORR_LEN].try_into().unwrap());
        (corr, frame.slice(CORR_LEN..))
    }
}

fn relay() -> Arc<Relay<FakeSignaling>> {
    let metrics = RelayMetrics::register(&Metrics::new().unwrap()).unwrap();
    Arc::new(Relay::new(FakeSignaling, metrics))
}

fn connect(relay: &Arc<Relay<FakeSignaling>>) -> Conn {
    let (tx, in_rx) = mpsc::channel(64);
    let (out_tx, rx) = mpsc::channel(64);
    let stream: FrameStream = Box::pin(in_rx);
    let sink: FrameSink =
        Box::pin(out_tx.sink_map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe)));
    tokio::spawn(Arc::clone(relay).serve(stream, sink));
    Conn { tx, rx }
}

#[tokio::test(start_paused = true)]
async fn replies_keep_the_request_correlation_id() {
    let relay = relay();
    let mut conn = connect(&relay);
    conn.send(7, b"hello").await;
    assert_eq!(conn.reply().await, (7, Bytes::from_static(b"ok:hello")));
    assert_eq!(relay.metrics.forwarded.get(), 1);
}

#[tokio::test(start_paused = true)]
async fn malformed_frames_are_dropped_and_the_connection_survives() {
    let relay = relay();
    let mut conn = connect(&relay);
    conn.send_raw(vec![0; CORR_LEN]).await;
    conn.send_raw(vec![0; MAX_REQUEST + 1]).await;
    conn.send(1, b"after").await;
    assert_eq!(conn.reply().await, (1, Bytes::from_static(b"ok:after")));
    assert_eq!(relay.metrics.dropped.get(), 2);
}

#[tokio::test(start_paused = true)]
async fn lost_upstream_replies_are_dropped_silently() {
    let relay = relay();
    let mut conn = connect(&relay);
    conn.send(1, b"lost").await;
    conn.send(2, b"next").await;
    assert_eq!(conn.reply().await, (2, Bytes::from_static(b"ok:next")));
    assert_eq!(relay.metrics.dropped.get(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_slow_request_does_not_block_pipelined_ones() {
    let relay = relay();
    let mut conn = connect(&relay);
    conn.send(1, b"slow").await;
    conn.send(2, b"fast").await;
    assert_eq!(conn.reply().await.0, 2);
    assert_eq!(conn.reply().await.0, 1);
}

#[tokio::test(start_paused = true)]
async fn connection_gauge_tracks_open_connections() {
    let relay = relay();
    let conn = connect(&relay);
    tokio::task::yield_now().await;
    assert_eq!(relay.metrics.connections.get(), 1);
    drop(conn);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(relay.metrics.connections.get(), 0);
}

#[tokio::test(start_paused = true)]
async fn frames_beyond_the_rate_limit_are_dropped() {
    let relay = relay();
    let mut conn = connect(&relay);
    // The bucket holds a two-second burst.
    for corr in 0..FRAMES_PER_SEC * 3 {
        conn.send(corr, b"burst").await;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        relay.metrics.dropped.get() > 0,
        "a burst over the limit is shed"
    );
    assert!(relay.metrics.forwarded.get() >= FRAMES_PER_SEC * 2);
}

#[tokio::test(start_paused = true)]
async fn idle_connections_are_closed() {
    let relay = relay();
    let _conn = connect(&relay);
    tokio::task::yield_now().await;
    assert_eq!(relay.metrics.connections.get(), 1);
    tokio::time::sleep(IDLE_TIMEOUT + Duration::from_secs(1)).await;
    assert_eq!(relay.metrics.connections.get(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_client_that_stops_reading_is_disconnected() {
    let relay = relay();
    let Conn { mut tx, rx } = connect(&relay);
    drop(rx);
    tx.send(Ok([1u64.to_le_bytes().as_slice(), b"x"].concat().into()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(relay.metrics.connections.get(), 0);
}

#[test]
fn refused_connections_are_counted() {
    let relay = relay();
    relay.rejected();
    assert_eq!(relay.metrics.rejected.get(), 1);
}
