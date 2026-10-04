//! Prometheus metrics owned by one service instance and exposed over a
//! minimal HTTP/1.1 endpoint, which also answers readiness on `/ready`.
//! Nothing is global, so several instances (e.g. in integration tests) never
//! collide.

use std::io::{Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_nats::connection::State;

use prometheus::core::{Collector, Desc};
use prometheus::proto::MetricFamily;
use prometheus::{Counter, Encoder, IntCounter, IntGauge, Registry, TextEncoder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::info;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct Metrics {
    registry: Registry,
    /// Set once the service accepts work; it stays ready while this bus
    /// connection is up.
    ready: Arc<OnceLock<async_nats::Client>>,
}

impl Metrics {
    /// A fresh registry with the tokio runtime's own metrics (`tokio_*`); on
    /// Linux it also exports process resident memory, CPU time and open file
    /// descriptors (`process_*`).
    pub fn new() -> anyhow::Result<Self> {
        let registry = Registry::new();
        registry.register(Box::new(RuntimeCollector::new()?))?;
        #[cfg(target_os = "linux")]
        registry.register(Box::new(
            prometheus::process_collector::ProcessCollector::for_self(),
        ))?;
        Ok(Self {
            registry,
            ready: Arc::default(),
        })
    }

    /// Marks the service ready: it has bound its listeners and connected
    /// its dependencies. From now on `/ready` answers 200 while `nats` stays
    /// connected, and 503 otherwise.
    pub fn set_ready(&self, nats: async_nats::Client) {
        let _ = self.ready.set(nats);
    }

    fn is_ready(&self) -> bool {
        self.ready
            .get()
            .is_some_and(|nats| nats.connection_state() == State::Connected)
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

    /// Binds `addr` and answers `/ready` with the service's readiness and
    /// any other request with [`Self::render`], until `shutdown`. Returns the
    /// bound address (useful with port 0).
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

/// Stable tokio runtime metrics, read on every scrape from the runtime the
/// scrape runs on. A task count that does not return to its baseline after
/// clients leave is a task leak.
struct RuntimeCollector {
    workers: IntGauge,
    alive_tasks: IntGauge,
    global_queue: IntGauge,
    busy: Counter,
    /// Busy time already added to `busy`, in nanoseconds.
    busy_seen: AtomicU64,
    descs: Vec<Desc>,
}

impl RuntimeCollector {
    fn new() -> prometheus::Result<Self> {
        let workers = IntGauge::new("tokio_workers", "Runtime worker threads")?;
        let alive_tasks = IntGauge::new("tokio_alive_tasks", "Tasks spawned and not yet finished")?;
        let global_queue = IntGauge::new(
            "tokio_global_queue_depth",
            "Tasks waiting in the runtime's global queue",
        )?;
        let busy = Counter::new(
            "tokio_worker_busy_seconds_total",
            "Time all workers spent running tasks",
        )?;
        let descs = [
            workers.desc(),
            alive_tasks.desc(),
            global_queue.desc(),
            busy.desc(),
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
        Ok(Self {
            workers,
            alive_tasks,
            global_queue,
            busy,
            busy_seen: AtomicU64::new(0),
            descs,
        })
    }

    fn sample(&self, metrics: &tokio::runtime::RuntimeMetrics) {
        let count = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
        self.workers.set(count(metrics.num_workers()));
        self.alive_tasks.set(count(metrics.num_alive_tasks()));
        self.global_queue.set(count(metrics.global_queue_depth()));
        let busy: Duration = (0..metrics.num_workers())
            .map(|w| metrics.worker_total_busy_duration(w))
            .sum();
        let nanos = u64::try_from(busy.as_nanos()).unwrap_or(u64::MAX);
        // fetch_max: concurrent scrapes each add only time nobody added yet.
        let seen = self.busy_seen.fetch_max(nanos, Ordering::Relaxed);
        self.busy
            .inc_by(Duration::from_nanos(nanos.saturating_sub(seen)).as_secs_f64());
    }
}

impl Collector for RuntimeCollector {
    fn desc(&self) -> Vec<&Desc> {
        self.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            self.sample(&runtime.metrics());
        }
        [
            self.workers.collect(),
            self.alive_tasks.collect(),
            self.global_queue.collect(),
            self.busy.collect(),
        ]
        .concat()
    }
}

async fn respond(mut stream: TcpStream, metrics: Metrics) {
    let mut request = [0u8; 1024];
    let Ok(Ok(read)) = tokio::time::timeout(REQUEST_TIMEOUT, stream.read(&mut request)).await
    else {
        return;
    };
    let (status, content_type, body) = if request.get(..read).is_some_and(asks_ready) {
        if metrics.is_ready() {
            ("200 OK", "text/plain", b"ready\n".to_vec())
        } else {
            (
                "503 Service Unavailable",
                "text/plain",
                b"not ready\n".to_vec(),
            )
        }
    } else {
        let Ok(body) = metrics.render() else {
            return;
        };
        ("200 OK", prometheus::TEXT_FORMAT, body)
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = tokio::time::timeout(REQUEST_TIMEOUT, async {
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(&body).await
    })
    .await;
}

fn asks_ready(request: &[u8]) -> bool {
    request.starts_with(b"GET /ready ")
}

/// Asks the service whose metrics listen on `addr` whether it is ready: the
/// container healthcheck, run as `<service> health` inside the image, which
/// has no HTTP client of its own.
pub fn probe_ready(addr: SocketAddr) -> anyhow::Result<()> {
    let ip = match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    let mut conn =
        std::net::TcpStream::connect_timeout(&SocketAddr::new(ip, addr.port()), PROBE_TIMEOUT)?;
    conn.set_read_timeout(Some(PROBE_TIMEOUT))?;
    conn.write_all(b"GET /ready HTTP/1.1\r\nConnection: close\r\n\r\n")?;
    let mut status = [0u8; 12];
    conn.read_exact(&mut status)?;
    anyhow::ensure!(&status == b"HTTP/1.1 200", "not ready");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ready only once the service says so, with its bus connected.
    #[tokio::test]
    async fn ready_once_the_service_says_so() {
        let metrics = Metrics::new().unwrap();
        let shutdown = CancellationToken::new();
        let addr = metrics
            .serve("0.0.0.0:0".parse().unwrap(), shutdown.clone())
            .await
            .unwrap();
        let probe = move || tokio::task::spawn_blocking(move || probe_ready(addr));
        assert!(
            probe().await.unwrap().is_err(),
            "not ready before it is set"
        );

        let Ok(url) = std::env::var("CYPHER_TEST_NATS") else {
            eprintln!("CYPHER_TEST_NATS unset; skipping the connected half");
            return;
        };
        let nats = async_nats::ConnectOptions::new()
            .user_and_password(
                "gateway".into(),
                std::env::var("GATEWAY_NATS_PASSWORD").unwrap_or_default(),
            )
            .connect(url)
            .await
            .unwrap();
        metrics.set_ready(nats);
        probe().await.unwrap().unwrap();
        shutdown.cancel();
    }

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runtime_metrics_follow_live_tasks() {
        let metrics = Metrics::new().unwrap();
        let text = || String::from_utf8(metrics.render().unwrap()).unwrap();
        let value = |name: &str| -> i64 {
            text()
                .lines()
                .find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')?.parse().ok())
                .unwrap()
        };
        assert_eq!(value("tokio_workers"), 2);
        let baseline = value("tokio_alive_tasks");
        let stop = CancellationToken::new();
        let tasks: Vec<_> = (0..5)
            .map(|_| tokio::spawn(stop.clone().cancelled_owned()))
            .collect();
        assert_eq!(value("tokio_alive_tasks"), baseline + 5);
        stop.cancel();
        for t in tasks {
            t.await.unwrap();
        }
        // A task is released by the runtime only after its JoinHandle has
        // resolved, so under load the count settles instead of dropping at once.
        let settled = async {
            while value("tokio_alive_tasks") != baseline {
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), settled)
            .await
            .expect("finished tasks are released");
        assert!(text().contains("tokio_worker_busy_seconds_total "));
    }

    #[test]
    fn runtime_metrics_are_absent_outside_a_runtime() {
        let text = String::from_utf8(Metrics::new().unwrap().render().unwrap()).unwrap();
        assert!(text.contains("tokio_alive_tasks 0"));
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
