//! Prometheus exposition over a minimal HTTP/1.1 endpoint.

use std::net::SocketAddr;
use std::time::Duration;

use prometheus::{Encoder, IntCounter, IntGauge, TextEncoder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, warn};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub fn counter(name: &str, help: &str) -> IntCounter {
    let c = IntCounter::new(name, help).expect("valid metric name");
    let _ = prometheus::register(Box::new(c.clone()));
    c
}

pub fn gauge(name: &str, help: &str) -> IntGauge {
    let g = IntGauge::new(name, help).expect("valid metric name");
    let _ = prometheus::register(Box::new(g.clone()));
    g
}

pub fn spawn_metrics_server(addr: SocketAddr) {
    tokio::spawn(async move {
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                warn!(%addr, "metrics server failed to bind: {e}");
                return;
            }
        };
        info!(%addr, "metrics server listening");
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                if tokio::time::timeout(REQUEST_TIMEOUT, stream.read(&mut buf))
                    .await
                    .is_err()
                {
                    return;
                }
                let mut body = Vec::new();
                let encoder = TextEncoder::new();
                if encoder.encode(&prometheus::gather(), &mut body).is_err() {
                    return;
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    encoder.format_type(),
                    body.len()
                );
                let _ = tokio::time::timeout(REQUEST_TIMEOUT, async {
                    stream.write_all(head.as_bytes()).await?;
                    stream.write_all(&body).await
                })
                .await;
            });
        }
    });
}
