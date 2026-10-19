//! What belongs to the unlocked identity besides its client, each part
//! owning its state and how it ends.

use cypher_client::Provisioning;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// This new device, waiting to be linked.
#[derive(Default)]
pub(crate) struct Linking(Mutex<LinkWait>);

#[derive(Default)]
struct LinkWait {
    waiting: Option<Provisioning>,
    /// Stays cancelled once cancelled, so a finish that has taken the wait
    /// but not started waiting yet still sees the cancel.
    cancel: CancellationToken,
}

impl Linking {
    /// Waits anew; a finish still waiting on an earlier offer stops.
    pub(crate) async fn start(&self, waiting: Provisioning) {
        let mut wait = self.0.lock().await;
        wait.cancel.cancel();
        *wait = LinkWait {
            waiting: Some(waiting),
            cancel: CancellationToken::new(),
        };
    }

    /// The wait to finish and what cancels it; `None` when not waiting.
    pub(crate) async fn take(&self) -> Option<(Provisioning, CancellationToken)> {
        let mut wait = self.0.lock().await;
        let waiting = wait.waiting.take()?;
        Some((waiting, wait.cancel.clone()))
    }

    /// Stops waiting, also a finish already under way.
    pub(crate) async fn cancel(&self) {
        let mut wait = self.0.lock().await;
        wait.cancel.cancel();
        wait.waiting = None;
    }
}
