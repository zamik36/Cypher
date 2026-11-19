use std::io;

use bytes::Bytes;
use futures::{Sink, SinkExt};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::outbox::{OutboxReceiver, Queued, Reservation};

use super::{WRITE_BATCH, WRITE_TIMEOUT};

/// Writes the outbox to the client until the session ends, then delivers
/// what is left and closes. A failed or stalled write ends the session.
pub(super) async fn write_loop<K>(mut sink: K, mut rx: OutboxReceiver, cancel: CancellationToken)
where
    K: Sink<Bytes, Error = io::Error> + Unpin,
{
    if write_until_closed(&mut sink, &mut rx, &cancel).await {
        drain_and_close(&mut sink, &mut rx).await;
    }
    cancel.cancel();
    debug!("writer closed");
}

/// Writes what is still queued and closes; a failed write leaves the rest,
/// and closing, undone.
async fn drain_and_close<K>(sink: &mut K, rx: &mut OutboxReceiver)
where
    K: Sink<Bytes, Error = io::Error> + Unpin,
{
    let mut held = None;
    while let Some(queued) = rx.try_recv() {
        if !feed(sink, queued, &mut held).await {
            return;
        }
    }
    written(sink.close()).await;
}

/// Writes queued frames in batches, one flush each. True when the session
/// ended, false when a write failed or stalled.
async fn write_until_closed<K>(
    sink: &mut K,
    rx: &mut OutboxReceiver,
    cancel: &CancellationToken,
) -> bool
where
    K: Sink<Bytes, Error = io::Error> + Unpin,
{
    loop {
        let first = tokio::select! {
            () = cancel.cancelled() => return true,
            f = rx.recv() => match f {
                Some(f) => f,
                None => return true,
            },
        };
        // The batch's bytes stay counted until it is flushed.
        let mut held = None;
        if !feed(sink, first, &mut held).await {
            return false;
        }
        for _ in 0..WRITE_BATCH {
            let Some(next) = rx.try_recv() else { break };
            if !feed(sink, next, &mut held).await {
                return false;
            }
        }
        if !written(sink.flush()).await {
            return false;
        }
    }
}

/// Writes a queued frame, adding its bytes to those `held` until the next
/// flush.
async fn feed<K>(sink: &mut K, queued: Queued, held: &mut Option<Reservation>) -> bool
where
    K: Sink<Bytes, Error = io::Error> + Unpin,
{
    let (frame, reservation) = queued.into_parts();
    match held {
        Some(held) => held.merge(reservation),
        None => *held = Some(reservation),
    }
    written(sink.feed(frame)).await
}

/// Runs one write to the client for at most `WRITE_TIMEOUT`; false when it
/// failed or stalled.
async fn written(write: impl Future<Output = io::Result<()>>) -> bool {
    matches!(timeout(WRITE_TIMEOUT, write).await, Ok(Ok(())))
}
