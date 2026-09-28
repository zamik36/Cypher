//! Gateway load generator: pairs of authenticated clients relaying frames
//! through one or two gateways. Reports connect and `Send` → `SendAck`
//! latency percentiles, throughput and the gateway's memory per connection,
//! and fails on the thresholds it is given.

mod args;
mod client;
mod probe;
mod report;
mod stats;

use std::io::Write as _;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use clap::Parser;

use args::Args;
use client::{PairOutcome, Plan, Stage};
use report::{Limits, Report};
use stats::Percentiles;

/// Lets the last connections of the ramp finish before memory is sampled.
const SETTLE: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() -> ExitCode {
    match run(Args::parse()).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            let _ = writeln!(std::io::stderr().lock(), "load-test: {e:#}");
            ExitCode::from(2)
        }
    }
}

async fn run(args: Args) -> Result<bool> {
    let tls = match &args.ca_cert {
        Some(path) => cypher_tls::make_client_config_with_pem(&std::fs::read_to_string(path)?)?,
        None => cypher_tls::make_client_config(),
    };
    let rss_before = rss(args.metrics_addr.as_deref()).await;
    let pairs = args.connections.div_ceil(2).max(1);
    let ramp = Duration::from_secs(args.ramp);
    let started = Instant::now();
    let plan = Arc::new(Plan {
        peer_addr: args
            .peer_gateway_addr
            .clone()
            .unwrap_or_else(|| args.gateway_addr.clone()),
        addr: args.gateway_addr.clone(),
        tls,
        interval: Duration::from_secs(1) / args.msg_rate.max(1),
        payload: Bytes::from(vec![0xA5u8; args.payload]),
        load_end: started + ramp + Duration::from_secs(args.duration),
    });

    let spacing = ramp / u32::try_from(pairs).unwrap_or(u32::MAX);
    let mut tasks = Vec::with_capacity(pairs);
    for i in 0..pairs {
        let src = args.src_ips.get(i % args.src_ips.len().max(1)).copied();
        let plan = Arc::clone(&plan);
        tasks.push(tokio::spawn(
            async move { client::run_pair(&plan, src).await },
        ));
        tokio::time::sleep_until(
            (started + spacing * u32::try_from(i + 1).unwrap_or(u32::MAX)).into(),
        )
        .await;
    }
    tokio::time::sleep(SETTLE).await;
    let rss_after = rss(args.metrics_addr.as_deref()).await;

    let mut outcomes = Vec::with_capacity(pairs);
    for task in tasks {
        outcomes.push(task.await.unwrap_or_else(|_| PairOutcome {
            failed: Some(Stage::Relay),
            ..PairOutcome::default()
        }));
    }
    let report = summarize(&args, outcomes, started.elapsed(), rss_before, rss_after);
    print(&args, &report)?;
    let limits = Limits {
        max_errors: args.max_errors,
        p99_ms: args.assert_p99_ms,
        bytes_per_conn: args.assert_max_bytes_per_conn,
    };
    let violations = report.violations(&limits);
    let mut err = std::io::stderr().lock();
    for v in &violations {
        writeln!(err, "FAIL: {v}")?;
    }
    Ok(violations.is_empty())
}

async fn rss(addr: Option<&str>) -> Option<u64> {
    let bytes = probe::resident_bytes(addr?).await;
    bytes
        .inspect_err(|e| tracing::warn!("gateway memory not measured: {e:#}"))
        .ok()
}

fn summarize(
    args: &Args,
    outcomes: Vec<PairOutcome>,
    elapsed: Duration,
    rss_before: Option<u64>,
    rss_after: Option<u64>,
) -> Report {
    let (mut connect_us, mut relay_us) = (Vec::new(), Vec::new());
    let (mut connected, mut on_primary) = (0u64, 0u64);
    let (mut connect_errors, mut relay_errors) = (0u64, 0u64);
    for o in outcomes {
        connected = connected.saturating_add(o.connected);
        on_primary = on_primary.saturating_add(o.on_primary);
        match o.failed {
            Some(Stage::Connect) => connect_errors = connect_errors.saturating_add(1),
            Some(Stage::Relay) => relay_errors = relay_errors.saturating_add(1),
            None => {}
        }
        connect_us.extend(o.connect_us);
        relay_us.extend(o.relay_us);
    }
    let relayed = relay_us.len() as u64;
    Report {
        connections: args.connections,
        connected,
        gateway_connections: on_primary,
        connect_errors,
        relay_errors,
        relayed,
        relayed_per_sec: relayed / elapsed.as_secs().max(1),
        connect_us: Percentiles::of(connect_us),
        relay_us: Percentiles::of(relay_us),
        gateway_rss_before: rss_before,
        gateway_rss_after: rss_after,
        bytes_per_connection: Report::per_connection(rss_before, rss_after, on_primary),
    }
}

fn print(args: &Args, report: &Report) -> Result<()> {
    let text = if args.json {
        serde_json::to_string_pretty(report)? + "\n"
    } else {
        report.render()?
    };
    std::io::stdout().lock().write_all(text.as_bytes())?;
    Ok(())
}
