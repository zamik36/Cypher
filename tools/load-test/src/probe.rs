//! Reads the gateway's resident memory and live task count from its
//! Prometheus endpoint.

use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout};

const TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(250);

/// One scrape of the gateway's metrics.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Snapshot {
    /// Linux only: the process collector.
    pub resident_bytes: Option<u64>,
    pub alive_tasks: Option<u64>,
}

pub(crate) async fn scrape(addr: &str) -> Result<Snapshot> {
    let mut stream = timeout(TIMEOUT, TcpStream::connect(addr))
        .await
        .context("metrics endpoint timed out")??;
    let request = format!("GET /metrics HTTP/1.0\r\nHost: {addr}\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut body = String::new();
    timeout(TIMEOUT, stream.read_to_string(&mut body))
        .await
        .context("metrics endpoint timed out")??;
    Ok(Snapshot {
        resident_bytes: gauge(&body, "process_resident_memory_bytes"),
        alive_tasks: gauge(&body, "tokio_alive_tasks"),
    })
}

/// Polls until the gateway runs at most `limit` tasks (its connections'
/// tasks have ended) or `within` passes; returns the last count seen.
pub(crate) async fn tasks_settled(addr: &str, limit: u64, within: Duration) -> Option<u64> {
    let deadline = Instant::now() + within;
    loop {
        let tasks = scrape(addr).await.ok()?.alive_tasks?;
        if tasks <= limit || Instant::now() >= deadline {
            return Some(tasks);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// A gauge's value; Prometheus prints them as floats, e.g. `1.2345e+08`.
fn gauge(text: &str, name: &str) -> Option<u64> {
    let value: f64 = text
        .lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(' '))?
        .trim()
        .parse()
        .ok()?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked finite and non-negative; counts and bytes fit in u64"
    )]
    let count = (value.is_finite() && value >= 0.0).then_some(value as u64);
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_gauges_by_exact_name() {
        let text = "HTTP/1.1 200 OK\r\n\r\n# HELP x\nprocess_resident_memory_bytes 1.2345e+08\n\
                    tokio_alive_tasks_extra 9\ntokio_alive_tasks 42\n";
        assert_eq!(
            gauge(text, "process_resident_memory_bytes"),
            Some(123_450_000)
        );
        assert_eq!(gauge(text, "tokio_alive_tasks"), Some(42));
        assert_eq!(
            gauge(
                "process_resident_memory_bytes NaN\n",
                "process_resident_memory_bytes"
            ),
            None
        );
        assert_eq!(gauge("gateway_connections 3\n", "tokio_alive_tasks"), None);
    }
}
