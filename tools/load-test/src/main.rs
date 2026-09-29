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
use tokio::task::JoinHandle;

use args::Args;
use client::{PairOutcome, Plan, Stage};
use probe::Snapshot;
use report::{Limits, Report};
use stats::Percentiles;

/// Lets the last connections of the ramp finish before memory is sampled.
const SETTLE: Duration = Duration::from_secs(1);
/// How long the gateway may take to notice every client left.
const DRAIN: Duration = Duration::from_secs(15);

/// What the gateway's metrics said around the run.
#[derive(Debug, Default)]
struct Gateway {
    before: Snapshot,
    after_ramp: Snapshot,
    tasks_after_close: Option<u64>,
}

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
    let mut gateway = Gateway {
        before: snapshot(args.metrics_addr.as_deref()).await,
        ..Gateway::default()
    };
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

    let tasks = ramp_up(&args, &plan, pairs, started + ramp).await;
    tokio::time::sleep(SETTLE).await;
    gateway.after_ramp = snapshot(args.metrics_addr.as_deref()).await;
    let outcomes = join(tasks).await;
    let elapsed = started.elapsed();
    if let (Some(addr), Some(base)) = (args.metrics_addr.as_deref(), gateway.before.alive_tasks) {
        let limit = base.saturating_add(args.assert_tasks_return.unwrap_or(0));
        gateway.tasks_after_close = probe::tasks_settled(addr, limit, DRAIN).await;
    }
    let report = summarize(&args, outcomes, elapsed, &gateway);
    print(&args, &report)?;
    let limits = Limits::from(&args);
    let violations = report.violations(&limits);
    let mut err = std::io::stderr().lock();
    for v in &violations {
        writeln!(err, "FAIL: {v}")?;
    }
    Ok(violations.is_empty())
}

/// Starts one pair per spacing interval so all are connecting by `ramp_end`.
async fn ramp_up(
    args: &Args,
    plan: &Arc<Plan>,
    pairs: usize,
    ramp_end: Instant,
) -> Vec<JoinHandle<PairOutcome>> {
    let started = Instant::now();
    let spacing =
        ramp_end.saturating_duration_since(started) / u32::try_from(pairs).unwrap_or(u32::MAX);
    let mut tasks = Vec::with_capacity(pairs);
    for i in 0..pairs {
        let src = args.src_ips.get(i % args.src_ips.len().max(1)).copied();
        let plan = Arc::clone(plan);
        tasks.push(tokio::spawn(
            async move { client::run_pair(&plan, src).await },
        ));
        let next = started + spacing * u32::try_from(i + 1).unwrap_or(u32::MAX);
        tokio::time::sleep_until(next.into()).await;
    }
    tasks
}

/// A pair whose task panicked counts as failed while relaying.
async fn join(tasks: Vec<JoinHandle<PairOutcome>>) -> Vec<PairOutcome> {
    let mut outcomes = Vec::with_capacity(tasks.len());
    for task in tasks {
        outcomes.push(task.await.unwrap_or_else(|_| PairOutcome {
            failed: Some(Stage::Relay),
            ..PairOutcome::default()
        }));
    }
    outcomes
}

async fn snapshot(addr: Option<&str>) -> Snapshot {
    let Some(addr) = addr else {
        return Snapshot::default();
    };
    probe::scrape(addr)
        .await
        .inspect_err(|e| tracing::warn!("gateway metrics not read: {e:#}"))
        .unwrap_or_default()
}

fn summarize(
    args: &Args,
    outcomes: Vec<PairOutcome>,
    elapsed: Duration,
    gateway: &Gateway,
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
        gateway_rss_before: gateway.before.resident_bytes,
        gateway_rss_after: gateway.after_ramp.resident_bytes,
        bytes_per_connection: Report::per_connection(
            gateway.before.resident_bytes,
            gateway.after_ramp.resident_bytes,
            on_primary,
        ),
        gateway_tasks_before: gateway.before.alive_tasks,
        gateway_tasks_after_close: gateway.tasks_after_close,
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
