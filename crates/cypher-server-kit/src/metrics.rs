//! Prometheus metrics owned by one service instance and exposed over a
//! minimal HTTP/1.1 endpoint. Nothing is global, so several instances (e.g.
//! in integration tests) never collide.

use std::net::SocketAddr;
use std::time::Duration;

use prometheus::{Encoder, IntCounter, IntGauge, Registry, TextEncoder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::info;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct Metrics {
    registry: Registry,
}

impl Metrics {
    /// A fresh registry; on Linux it also exports process resident memory,
    /// CPU time and open file descriptors (`process_*`).
    pub fn new() -> anyhow::Result<Self> {
        let registry = Registry::new();
        #[cfg(target_os = "linux")]
        registry.register(Box::new(
            prometheus::process_collector::ProcessCollector::for_self(),
        ))?;
        Ok(Self { registry })
    }

    pub fn counter(&self, name: &str, help: &str) -> anyhow::Result<IntCounter> {
        let counter = IntCounter::new(name, help)?;
        self.registry.register(Box::new(counter.clone()))?;
        Ok(counter)
    }

    pub fn gauge(&self, name: &str, help: &str) -> anyhow::Result<IntGauge> {
        let gauge = IntGauge::new(name, help)?;
        self.registry.register(Box::new(gauge.clone()))?;
        Ok(gauge)
    }

    /// Text exposition of every registered metric.
    pub fn render(&self) -> anyhow::Result<Vec<u8>> {
        let mut body = Vec::new();
        TextEncoder::new().encode(&self.registry.gather(), &mut body)?;
        Ok(body)
    }

    /// Binds `addr` and answers every request with [`Self::render`] until
    /// `shutdown`. Returns the bound address (useful with port 0).
    pub async fn serve(
        &self,
        addr: SocketAddr,
        shutdown: CancellationToken,
    ) -> std::io::Result<SocketAddr> {
        let listener = TcpListener::bind(addr).await?;
        let local = listener.local_addr()?;
        info!(addr = %local, "metrics server listening");
        let metrics = self.clone();
        tokio::spawn(async move {
            loop {
                let stream = tokio::select! {
                    () = shutdown.cancelled() => return,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => stream,
                        Err(_) => continue,
                    },
                };
                tokio::spawn(respond(stream, metrics.clone()));
            }
        });
        Ok(local)
    }
}

async fn respond(mut stream: TcpStream, metrics: Metrics) {
    let mut request = [0u8; 1024];
    if tokio::time::timeout(REQUEST_TIMEOUT, stream.read(&mut request))
        .await
        .is_err()
    {
        return;
    }
    let Ok(body) = metrics.render() else {
        return;
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        TextEncoder::new().format_type(),
        body.len()
    );
    let _ = tokio::time::timeout(REQUEST_TIMEOUT, async {
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(&body).await
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serves_its_own_registry_over_http() {
        let metrics = Metrics::new().unwrap();
        let hits = metrics.counter("test_hits_total", "Hits").unwrap();
        hits.inc_by(3);
        let shutdown = CancellationToken::new();
        let addr = metrics
            .serve("127.0.0.1:0".parse().unwrap(), shutdown.clone())
            .await
            .unwrap();

        let mut conn = TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"GET /metrics HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        conn.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("test_hits_total 3"));
        shutdown.cancel();
    }

    #[test]
    fn instances_are_independent() {
        let (a, b) = (Metrics::new().unwrap(), Metrics::new().unwrap());
        a.counter("dup_total", "d").unwrap();
        b.counter("dup_total", "d").unwrap();
        assert!(
            a.counter("dup_total", "d").is_err(),
            "duplicates are reported"
        );
    }
}
