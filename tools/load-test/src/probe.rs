//! Reads the gateway's resident memory from its Prometheus endpoint.

use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn resident_bytes(addr: &str) -> Result<u64> {
    let mut stream = timeout(TIMEOUT, TcpStream::connect(addr))
        .await
        .context("metrics endpoint timed out")??;
    let request = format!("GET /metrics HTTP/1.0\r\nHost: {addr}\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut body = String::new();
    timeout(TIMEOUT, stream.read_to_string(&mut body))
        .await
        .context("metrics endpoint timed out")??;
    parse_resident(&body)
        .context("no process_resident_memory_bytes (the process collector is Linux-only)")
}

/// Prometheus prints gauges as floats, e.g. `1.2345e+08`.
fn parse_resident(text: &str) -> Option<u64> {
    let value: f64 = text
        .lines()
        .find_map(|line| line.strip_prefix("process_resident_memory_bytes "))?
        .trim()
        .parse()
        .ok()?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked finite and non-negative; bytes fit in u64"
    )]
    let bytes = (value.is_finite() && value >= 0.0).then_some(value as u64);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_resident_memory_gauge() {
        let text =
            "HTTP/1.1 200 OK\r\n\r\n# HELP x\nprocess_resident_memory_bytes 1.2345e+08\nother 1\n";
        assert_eq!(parse_resident(text), Some(123_450_000));
        assert_eq!(
            parse_resident("process_resident_memory_bytes 4096\n"),
            Some(4096)
        );
        assert_eq!(parse_resident("process_resident_memory_bytes NaN\n"), None);
        assert_eq!(parse_resident("gateway_connections 3\n"), None);
    }
}
