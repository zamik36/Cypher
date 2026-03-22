//! Load testing tool for the P2P gateway.
//!
//! Opens N concurrent TLS connections, sends SESSION_INIT + heartbeat PINGs,
//! and reports connection rate, latency percentiles, and error counts.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use clap::Parser;
use tokio::sync::Semaphore;
use tracing::{error, info};

use cypher_proto::Serializable;
use cypher_transport::frame::FrameFlags;
use cypher_transport::TransportSession;

#[derive(Parser, Debug)]
#[command(name = "load-test", about = "P2P Gateway load testing tool")]
struct Args {
    /// Number of concurrent connections to open.
    #[arg(long, default_value = "100")]
    connections: usize,

    /// Duration to run the test in seconds.
    #[arg(long, default_value = "30")]
    duration: u64,

    /// Gateway address (host:port).
    #[arg(long, default_value = "127.0.0.1:9400")]
    gateway_addr: String,

    /// Maximum connections to open per second.
    #[arg(long, default_value = "50")]
    rate: usize,
}

struct Stats {
    connected: AtomicU64,
    errors: AtomicU64,
    pongs_received: AtomicU64,
    latencies_us: tokio::sync::Mutex<Vec<u64>>,
}

impl Stats {
    fn new() -> Self {
        Self {
            connected: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            pongs_received: AtomicU64::new(0),
            latencies_us: tokio::sync::Mutex::new(Vec::new()),
        }
    }
}

async fn run_client(gateway_addr: String, client_id: u64, stats: Arc<Stats>, deadline: Instant) {
    let connect_start = Instant::now();

    let tls_config = cypher_tls::make_client_config();

    let mut session = match TransportSession::connect(&gateway_addr, tls_config).await {
        Ok(s) => s,
        Err(e) => {
            error!(client_id, "connect error: {}", e);
            stats.errors.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    let connect_us = connect_start.elapsed().as_micros() as u64;
    stats.latencies_us.lock().await.push(connect_us);
    stats.connected.fetch_add(1, Ordering::Relaxed);

    let init = cypher_proto::SessionInit {
        client_id: client_id.to_le_bytes().to_vec(),
        nonce: vec![0u8; 32],
    };
    let payload = Bytes::from(init.serialize());
    if let Err(e) = session.send_frame(payload, FrameFlags::SESSION_INIT).await {
        error!(client_id, "send_frame error: {}", e);
        stats.errors.fetch_add(1, Ordering::Relaxed);
        return;
    }

    // Keep connection alive with PINGs until deadline.
    while Instant::now() < deadline {
        let ping_start = Instant::now();
        if let Err(e) = session.send_frame(Bytes::new(), FrameFlags::PING).await {
            error!(client_id, "ping send error: {}", e);
            stats.errors.fetch_add(1, Ordering::Relaxed);
            break;
        }

        match tokio::time::timeout(Duration::from_secs(5), session.recv_frame()).await {
            Ok(Ok(frame)) => {
                if frame.flags.contains(FrameFlags::PONG) {
                    let latency = ping_start.elapsed().as_micros() as u64;
                    stats.latencies_us.lock().await.push(latency);
                    stats.pongs_received.fetch_add(1, Ordering::Relaxed);
                }
            }
            Ok(Err(e)) => {
                error!(client_id, "recv error: {}", e);
                stats.errors.fetch_add(1, Ordering::Relaxed);
                break;
            }
            Err(_) => {
                error!(client_id, "ping timeout");
                stats.errors.fetch_add(1, Ordering::Relaxed);
                break;
            }
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // Graceful close.
    let _ = session.close().await;
}

#[tokio::main]
async fn main() -> Result<()> {
    cypher_common::init_tracing();
    let args = Args::parse();

    info!(
        connections = args.connections,
        duration_secs = args.duration,
        gateway = %args.gateway_addr,
        rate = args.rate,
        "Starting load test"
    );

    let stats = Arc::new(Stats::new());
    let deadline = Instant::now() + Duration::from_secs(args.duration);

    // Rate-limit connection creation.
    let semaphore = Arc::new(Semaphore::new(args.rate));
    let mut handles = Vec::new();

    for i in 0..args.connections {
        let permit = semaphore.clone().acquire_owned().await?;
        let stats = stats.clone();
        let addr = args.gateway_addr.clone();

        let handle = tokio::spawn(async move {
            run_client(addr, i as u64, stats, deadline).await;
            // Release permit after a short delay to enforce rate.
            tokio::time::sleep(Duration::from_millis(1000 / 50)).await;
            drop(permit);
        });
        handles.push(handle);

        if Instant::now() >= deadline {
            break;
        }
    }

    // Wait for all clients to finish.
    for h in handles {
        let _ = h.await;
    }

    // Print results.
    let connected = stats.connected.load(Ordering::Relaxed);
    let errors = stats.errors.load(Ordering::Relaxed);
    let pongs = stats.pongs_received.load(Ordering::Relaxed);
    let mut latencies = stats.latencies_us.lock().await;
    latencies.sort();

    println!("\n=== Load Test Results ===");
    println!("Connections attempted: {}", args.connections);
    println!("Connections succeeded: {}", connected);
    println!("Errors:               {}", errors);
    println!("Pongs received:       {}", pongs);

    if !latencies.is_empty() {
        let p50 = latencies[latencies.len() / 2];
        let p95 = latencies[latencies.len() * 95 / 100];
        let p99 = latencies[latencies.len() * 99 / 100];
        println!("Latency p50:          {} µs", p50);
        println!("Latency p95:          {} µs", p95);
        println!("Latency p99:          {} µs", p99);
    }

    let rate = connected as f64 / args.duration as f64;
    println!("Connections/sec:      {:.1}", rate);

    Ok(())
}
