use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cypher_transport::Conn;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio::time::timeout;

const OUTBOUND_FRAMES: usize = 256;

/// Network events waiting for the driver. When they pile up the readers stop
/// reading, so a fast sender is slowed down by TCP instead of filling memory.
const INBOUND_EVENTS: usize = 256;

/// How long one write to the server may take. A server that stops reading
/// for this long is treated as gone: the connection is dropped, which also
/// frees a driver waiting to queue a frame for it.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

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

/// The queue network tasks report to the driver through.
pub(crate) fn inbound() -> (mpsc::Sender<NetEvent>, mpsc::Receiver<NetEvent>) {
    mpsc::channel(INBOUND_EVENTS)
}

/// Connects directly over TLS, then pumps frames until either side closes.
pub(crate) fn spawn(
    link: Link,
    addr: String,
    tls: Arc<rustls::ClientConfig>,
    events: mpsc::Sender<NetEvent>,
) {
    tokio::spawn(async move {
        match cypher_transport::connect_tls(&addr, tls).await {
            Ok(conn) => pump(link, conn, events).await,
            Err(e) => {
                tracing::warn!(?link, %addr, "connect failed: {e}");
                let _ = events.send(NetEvent::Down(link)).await;
            }
        }
    });
}

/// Moves frames between an established connection and the driver, until
/// the server closes, the driver drops the sender handed to it, or a write
/// stalls.
pub(crate) async fn pump<S>(link: Link, conn: Conn<S>, events: mpsc::Sender<NetEvent>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = conn.split();
    let (tx, mut rx) = mpsc::channel::<Bytes>(OUTBOUND_FRAMES);
    if events.send(NetEvent::Up(link, tx)).await.is_err() {
        return;
    }
    let mut writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if !write(sink.feed(frame)).await {
                return;
            }
            while let Ok(frame) = rx.try_recv() {
                if !write(sink.feed(frame)).await {
                    return;
                }
            }
            if !write(sink.flush()).await {
                return;
            }
        }
        let _ = write(sink.close()).await;
    });
    // Waiting for room in `events` is what stops the reading: frames stay in
    // the socket, and the server's flow control slows the sender down.
    let reader = async {
        while let Some(Ok(frame)) = stream.next().await {
            if events
                .send(NetEvent::Frame(link, frame.freeze()))
                .await
                .is_err()
            {
                break;
            }
        }
    };
    // A frame still waiting for room when the writer gives up is lost with
    // the connection, like any frame in flight on it.
    tokio::select! {
        () = reader => {}
        _ = &mut writer => {}
    }
    writer.abort();
    let _ = events.send(NetEvent::Down(link)).await;
}

/// Runs one write for at most [`WRITE_TIMEOUT`]; false when it failed.
async fn write(op: impl Future<Output = Result<(), std::io::Error>>) -> bool {
    matches!(timeout(WRITE_TIMEOUT, op).await, Ok(Ok(())))
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use super::*;

    /// Frames nobody takes stay in the socket: the sender is held back
    /// until the driver catches up, then everything arrives in order.
    #[tokio::test]
    async fn a_reader_the_driver_does_not_keep_up_with_stops_reading() {
        let frames = u16::try_from(4 * INBOUND_EVENTS).unwrap();
        let (client, server) = tokio::io::duplex(4096);
        let (events, mut inbox) = inbound();
        tokio::spawn(pump(
            Link::Gateway,
            cypher_transport::framed(client),
            events,
        ));
        let mut sender = tokio::spawn(async move {
            let mut server = cypher_transport::framed(server);
            for n in 0..frames {
                server.send(Bytes::from(n.to_be_bytes().repeat(32))).await?;
            }
            server.get_mut().shutdown().await
        });

        // Held for the whole test: dropping it closes the connection.
        let Some(NetEvent::Up(Link::Gateway, _out)) = inbox.recv().await else {
            panic!("no Up first");
        };
        assert!(
            timeout(Duration::from_millis(300), &mut sender)
                .await
                .is_err(),
            "the sender is held back while nobody reads"
        );
        for n in 0..frames {
            let Some(NetEvent::Frame(Link::Gateway, frame)) = inbox.recv().await else {
                panic!("frame {n} missing");
            };
            assert_eq!(frame.get(..2), Some(n.to_be_bytes().as_slice()));
        }
        sender.await.unwrap().unwrap();
        assert!(matches!(
            inbox.recv().await,
            Some(NetEvent::Down(Link::Gateway))
        ));
    }
}
