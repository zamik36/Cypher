use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::VerifyingKey;
use rand::Rng;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use cypher_common::Result;

use super::fetcher::PipelinedFetcher;
use super::pool::TransportPool;

/// Power-aware mode controlling cover traffic frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerMode {
    /// Desktop: cover fetches every 2-5 min.
    Desktop,
    /// Mobile foreground: reduced frequency, 5-10 min.
    MobileForeground,
    /// Mobile background: OFF (OS suspends app).
    MobileBackground,
    /// Battery saver: OFF (user opted for non-anonymous direct mode).
    BatterySaver,
}

/// Background cover traffic generator — sends dummy inbox fetches at random
/// intervals to prevent timing analysis of real fetches.
pub struct CoverTraffic {
    pool: Arc<TransportPool>,
    inbox_verifying_key: VerifyingKey,
    mode: PowerMode,
}

impl CoverTraffic {
    pub fn new(
        pool: Arc<TransportPool>,
        inbox_verifying_key: VerifyingKey,
        mode: PowerMode,
    ) -> Self {
        Self {
            pool,
            inbox_verifying_key,
            mode,
        }
    }

    pub fn set_mode(&mut self, mode: PowerMode) {
        self.mode = mode;
    }

    /// Run the cover traffic loop until cancelled.
    ///
    /// Returns `Ok(())` on cancellation.
    pub async fn run(&self, cancel: CancellationToken) -> Result<()> {
        loop {
            let interval = match self.mode {
                PowerMode::Desktop => random_interval(120, 300),
                PowerMode::MobileForeground => random_interval(300, 600),
                // OFF modes: sleep forever (wake only on cancel).
                PowerMode::MobileBackground | PowerMode::BatterySaver => {
                    tokio::select! {
                        _ = cancel.cancelled() => return Ok(()),
                        // Wake every 60s to re-check mode (in case it changed).
                        _ = tokio::time::sleep(Duration::from_secs(60)) => continue,
                    }
                }
            };

            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                _ = tokio::time::sleep(interval) => {}
            }

            // Send 1-3 dummy inbox fetches.
            let count = rand::thread_rng().gen_range(1..=3);
            let dummy_ids: Vec<Vec<u8>> = (0..count)
                .map(|_| rand::random::<[u8; 32]>().to_vec())
                .collect();

            let fetcher = PipelinedFetcher::new(self.pool.clone(), self.inbox_verifying_key);
            match fetcher.fetch_all(dummy_ids).await {
                Ok(_) => debug!("cover traffic: sent {count} dummy fetches"),
                Err(e) => debug!("cover traffic: fetch failed (expected): {e}"),
            }
        }
    }
}

fn random_interval(min_secs: u64, max_secs: u64) -> Duration {
    let secs = rand::thread_rng().gen_range(min_secs..=max_secs);
    Duration::from_secs(secs)
}
