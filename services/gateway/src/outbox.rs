use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use tokio::sync::mpsc;

/// Per-connection bytes a slow reader may have queued before new deliveries
/// are refused (`Busy`). Bounds gateway memory per connection.
pub(crate) const QUEUE_BYTES: usize = 8 << 20;
const QUEUE_FRAMES: usize = 1024;

/// Bounded outbound queue shared by everything that writes to one client.
#[derive(Clone)]
pub struct Outbox {
    tx: mpsc::Sender<Bytes>,
    queued: Arc<AtomicUsize>,
}

pub(crate) struct OutboxReceiver {
    rx: mpsc::Receiver<Bytes>,
    queued: Arc<AtomicUsize>,
}

pub(crate) fn outbox() -> (Outbox, OutboxReceiver) {
    let (tx, rx) = mpsc::channel(QUEUE_FRAMES);
    let queued = Arc::new(AtomicUsize::new(0));
    (
        Outbox {
            tx,
            queued: Arc::clone(&queued),
        },
        OutboxReceiver { rx, queued },
    )
}

impl Outbox {
    /// Never waits: returns false when the reader is too slow or gone.
    pub fn try_push(&self, frame: Bytes) -> bool {
        let len = frame.len();
        if self.queued.fetch_add(len, Ordering::AcqRel) + len > QUEUE_BYTES {
            self.queued.fetch_sub(len, Ordering::AcqRel);
            return false;
        }
        if self.tx.try_send(frame).is_err() {
            self.queued.fetch_sub(len, Ordering::AcqRel);
            return false;
        }
        true
    }
}

impl OutboxReceiver {
    pub(crate) async fn recv(&mut self) -> Option<Bytes> {
        let frame = self.rx.recv().await?;
        self.queued.fetch_sub(frame.len(), Ordering::AcqRel);
        Some(frame)
    }

    pub(crate) fn try_recv(&mut self) -> Option<Bytes> {
        let frame = self.rx.try_recv().ok()?;
        self.queued.fetch_sub(frame.len(), Ordering::AcqRel);
        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn byte_budget_is_enforced_and_released() {
        let (tx, mut rx) = outbox();
        let big = Bytes::from(vec![0u8; QUEUE_BYTES / 2]);
        assert!(tx.try_push(big.clone()));
        assert!(tx.try_push(big.clone()));
        assert!(!tx.try_push(Bytes::from_static(b"x")));
        rx.recv().await.unwrap();
        assert!(tx.try_push(Bytes::from_static(b"x")));
    }

    #[tokio::test]
    async fn closed_receiver_refuses() {
        let (tx, rx) = outbox();
        drop(rx);
        assert!(!tx.try_push(Bytes::from_static(b"x")));
    }
}
