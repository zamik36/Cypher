//! Gateway load generator: pairs of authenticated clients exchanging relayed
//! frames; reports connection success and Send→SendAck latency percentiles.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use clap::Parser;
use cypher_crypto::IdentityKeyPair;
use cypher_transport::ClientConn;
use cypher_types::{PeerId, SESSION_AUTH_CONTEXT};
use cypher_wire::{ClientMsg, Frame, PROTOCOL_VERSION, ServerMsg};
use futures::{SinkExt, StreamExt};
use tokio::sync::Mutex;

#[derive(Parser, Debug)]
#[command(name = "load-test", about = "Cypher gateway load generator")]
struct Args {
    /// Number of client connections (rounded up to an even number).
    #[arg(long, default_value_t = 100)]
    connections: usize,
    /// Test duration in seconds.
    #[arg(long, default_value_t = 30)]
    duration: u64,
    #[arg(long, default_value = "localhost:9100")]
    gateway_addr: String,
    /// Connect the receiving side of each pair here to exercise cross-node
    /// delivery through NATS.
    #[arg(long)]
    peer_gateway_addr: Option<String>,
    /// PEM certificate to pin (development gateways).
    #[arg(long)]
    ca_cert: Option<std::path::PathBuf>,
    /// New connections per second.
    #[arg(long, default_value_t = 200)]
    rate: u32,
    /// Relayed messages per second per client.
    #[arg(long, default_value_t = 5)]
    msg_rate: u32,
    #[arg(long, default_value_t = 256)]
    payload: usize,
}

#[derive(Default)]
struct Stats {
    connected: AtomicU64,
    errors: AtomicU64,
    sent: AtomicU64,
    latencies_us: Mutex<Vec<u64>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let tls = match &args.ca_cert {
        Some(path) => cypher_tls::make_client_config_with_pem(&std::fs::read_to_string(path)?)?,
        None => cypher_tls::make_client_config(),
    };
    let stats = Arc::new(Stats::default());
    let deadline = Instant::now() + Duration::from_secs(args.duration);
    let pairs = args.connections.div_ceil(2);
    let spacing = Duration::from_secs_f64(2.0 / f64::from(args.rate.max(1)));
    let payload = Bytes::from(vec![0xA5u8; args.payload]);

    let target = Arc::new(Target {
        peer_addr: args
            .peer_gateway_addr
            .clone()
            .unwrap_or_else(|| args.gateway_addr.clone()),
        addr: args.gateway_addr.clone(),
        tls,
        msg_rate: args.msg_rate,
        payload,
        deadline,
    });
    let mut tasks = Vec::with_capacity(pairs);
    for _ in 0..pairs {
        let (target, stats) = (Arc::clone(&target), Arc::clone(&stats));
        tasks.push(tokio::spawn(async move {
            if let Err(e) = run_pair(&target, &stats).await {
                stats.errors.fetch_add(1, Ordering::Relaxed);
                tracing::debug!("pair failed: {e:#}");
            }
        }));
        tokio::time::sleep(spacing).await;
    }
    for t in tasks {
        let _ = t.await;
    }
    report(&stats, args.duration).await?;
    Ok(())
}

struct Target {
    addr: String,
    peer_addr: String,
    tls: Arc<rustls::ClientConfig>,
    msg_rate: u32,
    payload: Bytes,
    deadline: Instant,
}

async fn run_pair(t: &Target, stats: &Stats) -> Result<()> {
    let (a_id, b_id) = (IdentityKeyPair::generate(), IdentityKeyPair::generate());
    let mut a = connect(&t.addr, Arc::clone(&t.tls), &a_id).await?;
    let b = connect(&t.peer_addr, Arc::clone(&t.tls), &b_id).await?;
    stats.connected.fetch_add(2, Ordering::Relaxed);
    let b_peer = b_id.peer_id();
    let sink_task = tokio::spawn(drain(b));

    let interval = Duration::from_secs_f64(1.0 / f64::from(t.msg_rate.max(1)));
    let mut req_id = 0u32;
    while Instant::now() < t.deadline {
        req_id += 1;
        let started = Instant::now();
        a.send(Frame::new(req_id, send(b_peer, t.payload.clone())).encode())
            .await?;
        loop {
            let raw = a.next().await.context("gateway closed")??;
            if let Ok(Frame {
                req_id: id,
                msg: ServerMsg::SendAck { .. },
            }) = Frame::<ServerMsg>::decode(raw.freeze())
                && id == req_id
            {
                break;
            }
        }
        stats.sent.fetch_add(1, Ordering::Relaxed);
        stats
            .latencies_us
            .lock()
            .await
            .push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
        tokio::time::sleep(interval.saturating_sub(started.elapsed())).await;
    }
    sink_task.abort();
    Ok(())
}

fn send(to: PeerId, body: Bytes) -> ClientMsg {
    ClientMsg::Send {
        to,
        want_ack: true,
        body,
    }
}

async fn drain(mut conn: ClientConn) {
    while let Some(Ok(_)) = conn.next().await {}
}

async fn connect(
    addr: &str,
    tls: Arc<rustls::ClientConfig>,
    id: &IdentityKeyPair,
) -> Result<ClientConn> {
    let mut conn = cypher_transport::connect_tls(addr, tls).await?;
    conn.send(
        Frame::new(
            0,
            ClientMsg::Hello {
                version: PROTOCOL_VERSION,
                peer: id.peer_id(),
            },
        )
        .encode(),
    )
    .await?;
    let nonce = match next(&mut conn).await? {
        ServerMsg::Challenge { nonce } => nonce,
        other => bail!("expected challenge, got {other:?}"),
    };
    let mut signed = SESSION_AUTH_CONTEXT.to_vec();
    signed.extend_from_slice(&nonce);
    let signature = id.sign(&signed).to_bytes();
    conn.send(Frame::new(0, ClientMsg::Auth { signature }).encode())
        .await?;
    match next(&mut conn).await? {
        ServerMsg::Ready => Ok(conn),
        other => bail!("expected ready, got {other:?}"),
    }
}

async fn next(conn: &mut ClientConn) -> Result<ServerMsg> {
    let raw = conn.next().await.context("gateway closed")??;
    Ok(Frame::<ServerMsg>::decode(raw.freeze())?.msg)
}

async fn report(stats: &Stats, secs: u64) -> Result<()> {
    let mut lat = stats.latencies_us.lock().await;
    lat.sort_unstable();
    let pct = |p: usize| percentile(&lat, p);
    let sent = stats.sent.load(Ordering::Relaxed);
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "connected:   {}",
        stats.connected.load(Ordering::Relaxed)
    )?;
    writeln!(out, "errors:      {}", stats.errors.load(Ordering::Relaxed))?;
    writeln!(out, "relayed:     {sent} ({}/s)", sent / secs.max(1))?;
    writeln!(
        out,
        "latency µs:  p50={} p90={} p99={} max={}",
        pct(50),
        pct(90),
        pct(99),
        lat.last().copied().unwrap_or(0)
    )?;
    Ok(())
}

/// The `p`-th percentile (0..=100) of sorted samples, nearest-rank.
fn percentile(sorted: &[u64], p: usize) -> u64 {
    let rank = sorted.len().saturating_mul(p) / 100;
    sorted
        .get(rank.min(sorted.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0)
}
