use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use tokio::sync::mpsc;

/// Per-connection bytes a slow reader may have queued before new deliveries
/// are refused (`Busy`). Equal to the file transfer window of the core
/// (`WINDOW_BYTES` in `cypher-core/src/transfer.rs`): a smaller queue would
/// turn every bulk transfer to a slower reader into `Busy` and retransmits.
/// The sum over all connections is bounded by the gateway's [`Budget`].
pub(crate) const QUEUE_BYTES: usize = 8 << 20;
const QUEUE_FRAMES: usize = 1024;

/// Bytes the queues of all connections of one gateway may hold together.
/// Sized against the 768 MiB container limit (`docker-compose.prod.yml`):
/// 20 000 idle connections take about 470 MiB (24 KB RSS each,
/// `docs/performance.md`), which leaves this much for queued frames plus
/// room for frames in flight and allocator slack. Without a global bound,
/// the 128 connections one address may hold could pin 1 GiB by not reading.
pub(crate) const OUTBOX_BUDGET: usize = 192 << 20;

/// Gateway-wide count of queued outbound bytes.
#[derive(Debug)]
pub(crate) struct Budget {
    used: AtomicUsize,
    limit: usize,
}

impl Budget {
    pub(crate) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            used: AtomicUsize::new(0),
            limit,
        })
    }

    #[cfg(test)]
    pub(crate) fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
}

/// What the halves of one outbox share: its own byte count and the budget
/// it draws from.
#[derive(Debug)]
struct Counters {
    queued: AtomicUsize,
    budget: Arc<Budget>,
}

impl Counters {
    /// Takes `len` bytes from both the connection's share and the budget.
    fn reserve(&self, len: usize) -> bool {
        if !take(&self.queued, len, QUEUE_BYTES) {
            return false;
        }
        if !take(&self.budget.used, len, self.budget.limit) {
            self.queued.fetch_sub(len, Ordering::AcqRel);
            return false;
        }
        true
    }

    fn release(&self, len: usize) {
        self.queued.fetch_sub(len, Ordering::AcqRel);
        self.budget.used.fetch_sub(len, Ordering::AcqRel);
    }
}

/// Adds `len` to `counter` unless that would pass `limit`.
fn take(counter: &AtomicUsize, len: usize, limit: usize) -> bool {
    if counter.fetch_add(len, Ordering::AcqRel).saturating_add(len) > limit {
        counter.fetch_sub(len, Ordering::AcqRel);
        return false;
    }
    true
}

/// Bounded outbound queue shared by everything that writes to one client.
#[derive(Clone)]
pub struct Outbox {
    tx: mpsc::Sender<Bytes>,
    counters: Arc<Counters>,
}

/// The writer's end. Dropping it returns whatever is still queued to the
/// budget.
pub(crate) struct OutboxReceiver {
    rx: mpsc::Receiver<Bytes>,
    counters: Arc<Counters>,
}

pub(crate) fn outbox(budget: &Arc<Budget>) -> (Outbox, OutboxReceiver) {
    let (tx, rx) = mpsc::channel(QUEUE_FRAMES);
    let counters = Arc::new(Counters {
        queued: AtomicUsize::new(0),
        budget: Arc::clone(budget),
    });
    (
        Outbox {
            tx,
            counters: Arc::clone(&counters),
        },
        OutboxReceiver { rx, counters },
    )
}

impl Outbox {
    /// Never waits: returns false when the reader is too slow or gone, or
    /// the gateway holds its whole budget of queued bytes.
    pub fn try_push(&self, frame: Bytes) -> bool {
        let len = frame.len();
        if !self.counters.reserve(len) {
            return false;
        }
        if self.tx.try_send(frame).is_err() {
            self.counters.release(len);
            return false;
        }
        true
    }

    /// Bytes queued for the client right now.
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        self.counters.queued.load(Ordering::Acquire)
    }
}

impl OutboxReceiver {
    pub(crate) async fn recv(&mut self) -> Option<Bytes> {
        let frame = self.rx.recv().await?;
        self.counters.release(frame.len());
        Some(frame)
    }

    pub(crate) fn try_recv(&mut self) -> Option<Bytes> {
        let frame = self.rx.try_recv().ok()?;
        self.counters.release(frame.len());
        Some(frame)
    }
}

impl Drop for OutboxReceiver {
    /// Closing first makes every later `try_push` fail and undo its own
    /// reservation, so draining releases exactly what was queued.
    fn drop(&mut self) {
        self.rx.close();
        while self.try_recv().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unbounded() -> Arc<Budget> {
        Budget::new(usize::MAX)
    }

    #[tokio::test]
    async fn byte_budget_is_enforced_and_released() {
        let (tx, mut rx) = outbox(&unbounded());
        let big = Bytes::from(vec![0u8; QUEUE_BYTES / 2]);
        assert!(tx.try_push(big.clone()));
        assert!(tx.try_push(big.clone()));
        assert!(!tx.try_push(Bytes::from_static(b"x")));
        rx.recv().await.unwrap();
        assert!(tx.try_push(Bytes::from_static(b"x")));
    }

    #[tokio::test]
    async fn closed_receiver_refuses() {
        let budget = unbounded();
        let (tx, rx) = outbox(&budget);
        drop(rx);
        assert!(!tx.try_push(Bytes::from_static(b"x")));
        assert_eq!(budget.used(), 0, "the refused frame took nothing");
    }

    /// Connections draw from one budget: once it is spent, every queue
    /// refuses, though each is far below its own share.
    #[tokio::test]
    async fn the_gateway_budget_bounds_all_queues_together() {
        let budget = Budget::new(3 * 1024);
        let frame = Bytes::from(vec![0u8; 1024]);
        let (a, mut a_rx) = outbox(&budget);
        let (b, _b_rx) = outbox(&budget);
        assert!(a.try_push(frame.clone()));
        assert!(a.try_push(frame.clone()));
        assert!(b.try_push(frame.clone()));
        assert!(!b.try_push(frame.clone()), "budget spent");
        assert!(!a.try_push(Bytes::from_static(b"x")));
        assert_eq!(a.queued() + b.queued(), budget.used());

        a_rx.recv().await.unwrap();
        assert!(b.try_push(frame), "a written frame returns its bytes");
    }

    #[tokio::test]
    async fn a_dropped_receiver_returns_its_queued_bytes() {
        let budget = unbounded();
        let (tx, rx) = outbox(&budget);
        assert!(tx.try_push(Bytes::from(vec![0u8; 4096])));
        assert!(tx.try_push(Bytes::from_static(b"x")));
        assert_eq!(budget.used(), 4097);
        drop(rx);
        assert_eq!(budget.used(), 0);
        assert_eq!(tx.queued(), 0);
    }
}
