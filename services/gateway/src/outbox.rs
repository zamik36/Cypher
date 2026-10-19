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
    fn reserve(self: &Arc<Self>, len: usize) -> Option<Reservation> {
        if !take(&self.queued, len, QUEUE_BYTES) {
            return None;
        }
        if !take(&self.budget.used, len, self.budget.limit) {
            self.queued.fetch_sub(len, Ordering::AcqRel);
            return None;
        }
        Some(Reservation {
            counters: Arc::clone(self),
            len,
        })
    }
}

/// Bytes a queued frame holds of its connection's share and of the budget.
/// They return when it is dropped, wherever that happens: once the frame is
/// written, on a refused send, or with the channel when the connection goes.
pub(crate) struct Reservation {
    counters: Arc<Counters>,
    len: usize,
}

impl Reservation {
    /// Takes over the bytes of `other`, from the same outbox, to return
    /// them together.
    pub(crate) fn merge(&mut self, mut other: Self) {
        debug_assert!(Arc::ptr_eq(&self.counters, &other.counters));
        self.len += std::mem::take(&mut other.len);
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.len == 0 {
            return;
        }
        self.counters.queued.fetch_sub(self.len, Ordering::AcqRel);
        self.counters
            .budget
            .used
            .fetch_sub(self.len, Ordering::AcqRel);
    }
}

/// A frame in the queue with the bytes it holds.
pub(crate) struct Queued {
    frame: Bytes,
    reservation: Reservation,
}

impl Queued {
    /// The frame to write, and its bytes to hold until it is written.
    pub(crate) fn into_parts(self) -> (Bytes, Reservation) {
        (self.frame, self.reservation)
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
    tx: mpsc::Sender<Queued>,
    counters: Arc<Counters>,
}

/// The writer's end.
pub(crate) struct OutboxReceiver {
    rx: mpsc::Receiver<Queued>,
}

pub(crate) fn outbox(budget: &Arc<Budget>) -> (Outbox, OutboxReceiver) {
    let (tx, rx) = mpsc::channel(QUEUE_FRAMES);
    let counters = Arc::new(Counters {
        queued: AtomicUsize::new(0),
        budget: Arc::clone(budget),
    });
    (Outbox { tx, counters }, OutboxReceiver { rx })
}

impl Outbox {
    /// Never waits: returns false when the reader is too slow or gone, or
    /// the gateway holds its whole budget of queued bytes.
    pub fn try_push(&self, frame: Bytes) -> bool {
        let Some(reservation) = self.counters.reserve(frame.len()) else {
            return false;
        };
        self.tx.try_send(Queued { frame, reservation }).is_ok()
    }

    /// Bytes queued for the client right now.
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        self.counters.queued.load(Ordering::Acquire)
    }
}

impl OutboxReceiver {
    pub(crate) async fn recv(&mut self) -> Option<Queued> {
        self.rx.recv().await
    }

    pub(crate) fn try_recv(&mut self) -> Option<Queued> {
        self.rx.try_recv().ok()
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

    /// Pushes racing the receiver's drop never strand bytes in the budget:
    /// whether a frame was refused, delivered or still queued, its bytes
    /// come back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_pushes_and_a_dropped_receiver_leave_nothing_reserved() {
        async fn push_many(tx: Outbox) {
            for _ in 0..64 {
                tx.try_push(Bytes::from_static(b"frame"));
                tokio::task::yield_now().await;
            }
        }
        let budget = unbounded();
        for _round in 0..200 {
            let (tx, mut rx) = outbox(&budget);
            let pushers: Vec<_> = (0..4)
                .map(|_| tokio::spawn(push_many(tx.clone())))
                .collect();
            drop(tx);
            for _ in 0..8 {
                rx.try_recv();
                tokio::task::yield_now().await;
            }
            drop(rx);
            for pusher in pushers {
                pusher.await.unwrap();
            }
        }
        assert_eq!(budget.used(), 0);
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
