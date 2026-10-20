use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cypher_transport::Conn;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tokio::time::timeout;

const OUTBOUND_FRAMES: usize = 256;

/// Network events waiting for the driver. When they pile up the readers stop
/// reading, so a fast sender is slowed down by TCP instead of filling memory.
const INBOUND_EVENTS: usize = 256;

/// How long one write to the server, or the driver's wait to queue a frame
/// for it, may take. A server that stops reading for this long is treated
/// as gone and its connection is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Link {
    Gateway,
    Relay,
}

/// One connection attempt on a link. The driver numbers its attempts and
/// trusts only the latest, so a late event from a connection it already
/// dropped cannot touch the one that replaced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Attempt {
    pub link: Link,
    pub epoch: u64,
}

/// Something that happened on a connection attempt.
pub(crate) struct NetEvent {
    pub from: Attempt,
    pub kind: NetKind,
}

pub(crate) enum NetKind {
    Up(mpsc::Sender<Bytes>),
    Frame(Bytes),
    Down,
}

impl Attempt {
    fn event(self, kind: NetKind) -> NetEvent {
        NetEvent { from: self, kind }
    }
}

/// The queue network tasks report to the driver through.
pub(crate) fn inbound() -> (mpsc::Sender<NetEvent>, mpsc::Receiver<NetEvent>) {
    mpsc::channel(INBOUND_EVENTS)
}

/// Connects directly over TLS, then pumps frames until either side closes.
/// Aborting the returned handle drops the attempt, connected or not.
pub(crate) fn spawn(
    attempt: Attempt,
    addr: String,
    tls: Arc<rustls::ClientConfig>,
    events: mpsc::Sender<NetEvent>,
) -> AbortHandle {
    tokio::spawn(async move {
        match cypher_transport::connect_tls(&addr, tls).await {
            Ok(conn) => pump(attempt, conn, events).await,
            Err(e) => {
                tracing::warn!(link = ?attempt.link, %addr, "connect failed: {e}");
                let _ = events.send(attempt.event(NetKind::Down)).await;
            }
        }
    })
    .abort_handle()
}

/// Moves frames between an established connection and the driver, until
/// the server closes, the driver drops the sender handed to it, or a write
/// stalls.
pub(crate) async fn pump<S>(attempt: Attempt, conn: Conn<S>, events: mpsc::Sender<NetEvent>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = conn.split();
    let (tx, mut rx) = mpsc::channel::<Bytes>(OUTBOUND_FRAMES);
    if events.send(attempt.event(NetKind::Up(tx))).await.is_err() {
        return;
    }
    // Runs inside this task rather than its own, so aborting the attempt
    // stops the writes too.
    let writer = async {
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
    };
    // Waiting for room in `events` is what stops the reading: frames stay in
    // the socket, and the server's flow control slows the sender down.
    let reader = async {
        while let Some(Ok(frame)) = stream.next().await {
            if events
                .send(attempt.event(NetKind::Frame(frame.freeze())))
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
        () = writer => {}
    }
    let _ = events.send(attempt.event(NetKind::Down)).await;
}

/// Hands `frame` to the connection behind `out`. False when the connection
/// did not take it within [`WRITE_TIMEOUT`]: it ended, or it stopped
/// writing; either way the caller should drop it.
pub(crate) async fn queue(out: &mpsc::Sender<Bytes>, frame: Bytes) -> bool {
    queue_within(out, frame, WRITE_TIMEOUT).await
}

async fn queue_within(out: &mpsc::Sender<Bytes>, frame: Bytes, limit: Duration) -> bool {
    match timeout(limit, out.reserve()).await {
        Ok(Ok(room)) => {
            room.send(frame);
            true
        }
        Ok(Err(_)) | Err(_) => false,
    }
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
        let attempt = Attempt {
            link: Link::Gateway,
            epoch: 1,
        };
        tokio::spawn(pump(attempt, cypher_transport::framed(client), events));
        let mut sender = tokio::spawn(async move {
            let mut server = cypher_transport::framed(server);
            for n in 0..frames {
                server.send(Bytes::from(n.to_be_bytes().repeat(32))).await?;
            }
            server.get_mut().shutdown().await
        });

        // Held for the whole test: dropping it closes the connection.
        let Some(NetEvent {
            from,
            kind: NetKind::Up(_out),
        }) = inbox.recv().await
        else {
            panic!("no Up first");
        };
        assert_eq!(from, attempt);
        assert!(
            timeout(Duration::from_millis(300), &mut sender)
                .await
                .is_err(),
            "the sender is held back while nobody reads"
        );
        for n in 0..frames {
            let Some(NetEvent {
                kind: NetKind::Frame(frame),
                ..
            }) = inbox.recv().await
            else {
                panic!("frame {n} missing");
            };
            assert_eq!(frame.get(..2), Some(n.to_be_bytes().as_slice()));
        }
        sender.await.unwrap().unwrap();
        assert!(matches!(
            inbox.recv().await,
            Some(NetEvent {
                kind: NetKind::Down,
                ..
            })
        ));
    }

    /// A frame waits for room only so long: a connection that takes none,
    /// or has ended, is reported as unable to.
    #[tokio::test]
    async fn queueing_gives_up_on_a_connection_that_takes_nothing() {
        let limit = Duration::from_millis(20);
        let (out, mut taken) = mpsc::channel(1);
        assert!(queue_within(&out, Bytes::from_static(b"a"), limit).await);
        assert!(!queue_within(&out, Bytes::from_static(b"b"), limit).await);
        assert_eq!(taken.recv().await, Some(Bytes::from_static(b"a")));
        assert!(queue_within(&out, Bytes::from_static(b"c"), limit).await);
        drop(taken);
        assert!(!queue_within(&out, Bytes::from_static(b"d"), limit).await);
    }

    /// Dropping an attempt closes its socket at once, even while the driver
    /// still holds the sender it was handed.
    #[tokio::test]
    async fn an_aborted_pump_closes_its_connection() {
        let (client, server) = tokio::io::duplex(4096);
        let (events, mut inbox) = inbound();
        let attempt = Attempt {
            link: Link::Relay,
            epoch: 3,
        };
        let pump = tokio::spawn(pump(attempt, cypher_transport::framed(client), events));
        let Some(NetEvent {
            kind: NetKind::Up(_out),
            ..
        }) = inbox.recv().await
        else {
            panic!("no Up first");
        };

        pump.abort();
        let mut server = cypher_transport::framed(server);
        let closed = timeout(Duration::from_secs(5), server.next()).await;
        assert!(
            matches!(closed, Ok(None | Some(Err(_)))),
            "the server sees the connection end"
        );
    }
}
