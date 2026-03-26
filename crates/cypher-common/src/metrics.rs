//! Minimal HTTP server that exposes Prometheus metrics on `/metrics`.
//!
//! Each service calls [`spawn_metrics_server`] once at startup with a
//! dedicated port (e.g. 9090 for gateway, 9091 for signaling, 9092 for relay).

use prometheus::{Encoder, TextEncoder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{debug, warn};

pub fn spawn_metrics_server(port: u16) {
    tokio::spawn(async move {
        let addr = format!("0.0.0.0:{}", port);
        let listener = match TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                warn!("metrics server failed to bind on {}: {}", addr, e);
                return;
            }
        };
        tracing::info!("Metrics server listening on {}", addr);

        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    debug!("metrics accept error: {}", e);
                    continue;
                }
            };

            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;

                let encoder = TextEncoder::new();
                let metric_families = prometheus::gather();
                let mut body = Vec::new();
                encoder.encode(&metric_families, &mut body).unwrap();

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n",
                    encoder.format_type(),
                    body.len(),
                );

                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.write_all(&body).await;
            });
        }
    });
}
