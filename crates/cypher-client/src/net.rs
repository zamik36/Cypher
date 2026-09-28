use std::sync::Arc;

use bytes::Bytes;
use cypher_transport::Conn;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

const OUTBOUND_FRAMES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Link {
    Gateway,
    Relay,
}

pub(crate) enum NetEvent {
    Up(Link, mpsc::Sender<Bytes>),
    Frame(Link, Bytes),
    Down(Link),
}

/// Connects directly over TLS, then pumps frames until either side closes.
pub(crate) fn spawn(
    link: Link,
    addr: String,
    tls: Arc<rustls::ClientConfig>,
    events: mpsc::UnboundedSender<NetEvent>,
) {
    tokio::spawn(async move {
        match cypher_transport::connect_tls(&addr, tls).await {
            Ok(conn) => pump(link, conn, events).await,
            Err(e) => {
                tracing::warn!(?link, %addr, "connect failed: {e}");
                let _ = events.send(NetEvent::Down(link));
            }
        }
    });
}

/// Moves frames between an established connection and the driver.
pub(crate) async fn pump<S>(link: Link, conn: Conn<S>, events: mpsc::UnboundedSender<NetEvent>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = conn.split();
    let (tx, mut rx) = mpsc::channel::<Bytes>(OUTBOUND_FRAMES);
    if events.send(NetEvent::Up(link, tx)).is_err() {
        return;
    }
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if sink.feed(frame).await.is_err() {
                return;
            }
            while let Ok(frame) = rx.try_recv() {
                if sink.feed(frame).await.is_err() {
                    return;
                }
            }
            if sink.flush().await.is_err() {
                return;
            }
        }
        let _ = sink.close().await;
    });
    while let Some(Ok(frame)) = stream.next().await {
        if events.send(NetEvent::Frame(link, frame.freeze())).is_err() {
            break;
        }
    }
    writer.abort();
    let _ = events.send(NetEvent::Down(link));
}
