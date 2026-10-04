//! Every service metric that a dashboard or alert names is one the services
//! export. Names drift silently otherwise: the first overview dashboard
//! showed nothing for months. Needs `CYPHER_TEST_REDIS` and
//! `CYPHER_TEST_NATS`; skipped otherwise.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::Path;

use e2e::Stack;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Prefixes of the metrics the services export themselves. Exporters
/// (redis_, gnatsd_, node_, probe_) are not checked here.
#[cfg(target_os = "linux")]
const OURS: &[&str] = &["gateway_", "signaling_", "relay_", "tokio_", "process_"];
/// `process_*` comes from the process collector, which exists only on Linux.
#[cfg(not(target_os = "linux"))]
const OURS: &[&str] = &["gateway_", "signaling_", "relay_", "tokio_"];

#[tokio::test(flavor = "multi_thread")]
async fn dashboards_and_alerts_name_only_exported_metrics() {
    let Some(stack) = Stack::from_env().await else {
        eprintln!("CYPHER_TEST_REDIS / CYPHER_TEST_NATS not set; skipping");
        return;
    };
    let mut exported = BTreeSet::new();
    for &addr in stack.metrics_addrs() {
        exported.extend(exported_names(&scrape(addr).await.unwrap()));
    }
    stack.stop().await;

    let deploy = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy");
    let mut sources = vec![deploy.join("alerts.yml")];
    for entry in std::fs::read_dir(deploy.join("grafana/dashboards")).unwrap() {
        sources.push(entry.unwrap().path());
    }
    for source in sources {
        let text = std::fs::read_to_string(&source).unwrap();
        let missing: Vec<_> = referenced_names(&text)
            .filter(|name| !exported.contains(*name))
            .collect();
        assert!(
            missing.is_empty(),
            "{} names metrics no service exports: {missing:?}",
            source.display()
        );
    }
}

async fn scrape(addr: SocketAddr) -> std::io::Result<String> {
    let mut conn = TcpStream::connect(addr).await?;
    conn.write_all(b"GET /metrics HTTP/1.1\r\nConnection: close\r\n\r\n")
        .await?;
    let mut response = String::new();
    conn.read_to_string(&mut response).await?;
    Ok(response)
}

/// Metric names in a Prometheus text exposition.
fn exported_names(exposition: &str) -> impl Iterator<Item = String> + '_ {
    exposition
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split(['{', ' ']).next())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// Words in `text` that look like one of our metric names.
fn referenced_names(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| OURS.iter().any(|prefix| word.starts_with(prefix)))
}
