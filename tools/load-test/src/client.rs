//! One pair of authenticated clients: the sender relays frames to the
//! receiver and times each `Send` → `SendAck` round trip.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use cypher_crypto::IdentityKeyPair;
use cypher_transport::ClientConn;
use cypher_types::SESSION_AUTH_CONTEXT;
use cypher_wire::{ClientMsg, Frame, PROTOCOL_VERSION, ServerMsg};
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpSocket, TcpStream, lookup_host};

use crate::stats::micros;

/// Where and how every pair connects and sends.
pub(crate) struct Plan {
    pub addr: String,
    pub peer_addr: String,
    pub tls: Arc<rustls::ClientConfig>,
    pub interval: Duration,
    pub payload: Bytes,
    pub load_end: Instant,
}

/// Which phase a failed pair died in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    Connect,
    Relay,
}

/// Samples of one pair, in microseconds.
#[derive(Default)]
pub(crate) struct PairOutcome {
    pub connect_us: Vec<u64>,
    pub relay_us: Vec<u64>,
    pub connected: u64,
    /// Connections held by the first gateway, the one whose memory is sampled.
    pub on_primary: u64,
    pub failed: Option<Stage>,
}

pub(crate) async fn run_pair(plan: &Plan, src: Option<IpAddr>) -> PairOutcome {
    let mut out = PairOutcome::default();
    let (a_id, b_id) = (IdentityKeyPair::generate(), IdentityKeyPair::generate());
    let a = timed_connect(&plan.addr, plan, src, &a_id, &mut out).await;
    out.on_primary += u64::from(a.is_some());
    let b = timed_connect(&plan.peer_addr, plan, src, &b_id, &mut out).await;
    if plan.peer_addr == plan.addr {
        out.on_primary += u64::from(b.is_some());
    }
    let (Some(mut a), Some(b)) = (a, b) else {
        out.failed = Some(Stage::Connect);
        return out;
    };
    let receiver = tokio::spawn(drain(b));
    if relay(&mut a, plan, b_id.peer_id(), &mut out.relay_us)
        .await
        .is_err()
    {
        out.failed = Some(Stage::Relay);
    }
    receiver.abort();
    out
}

async fn timed_connect(
    addr: &str,
    plan: &Plan,
    src: Option<IpAddr>,
    id: &IdentityKeyPair,
    out: &mut PairOutcome,
) -> Option<ClientConn> {
    let started = Instant::now();
    let conn = connect(addr, Arc::clone(&plan.tls), src, id).await;
    match conn {
        Ok(conn) => {
            out.connect_us.push(micros(started.elapsed()));
            out.connected += 1;
            Some(conn)
        }
        Err(e) => {
            tracing::debug!("connect to {addr} failed: {e:#}");
            None
        }
    }
}

/// Paced `Send`s until the load window closes, timing each acknowledgement.
async fn relay(
    conn: &mut ClientConn,
    plan: &Plan,
    to: cypher_types::PeerId,
    samples: &mut Vec<u64>,
) -> Result<()> {
    let mut req_id = 0u32;
    while Instant::now() < plan.load_end {
        req_id = req_id.wrapping_add(1);
        let started = Instant::now();
        let send = ClientMsg::Send {
            to,
            want_ack: true,
            body: plan.payload.clone(),
        };
        conn.send(Frame::new(req_id, send).encode()).await?;
        wait_ack(conn, req_id).await?;
        samples.push(micros(started.elapsed()));
        tokio::time::sleep(plan.interval.saturating_sub(started.elapsed())).await;
    }
    Ok(())
}

async fn wait_ack(conn: &mut ClientConn, req_id: u32) -> Result<()> {
    loop {
        let raw = conn.next().await.context("gateway closed")??;
        if let Ok(Frame {
            req_id: id,
            msg: ServerMsg::SendAck { .. },
        }) = Frame::<ServerMsg>::decode(raw.freeze())
            && id == req_id
        {
            return Ok(());
        }
    }
}

async fn drain(mut conn: ClientConn) {
    while let Some(Ok(_)) = conn.next().await {}
}

/// TCP (from `src` when given), TLS, then `Hello` → `Challenge` → `Auth`.
async fn connect(
    addr: &str,
    tls: Arc<rustls::ClientConfig>,
    src: Option<IpAddr>,
    id: &IdentityKeyPair,
) -> Result<ClientConn> {
    let (host, port) = cypher_transport::split_host_port(addr)?;
    let tcp = connect_tcp(host, port, src).await?;
    tcp.set_nodelay(true)?;
    let mut conn = cypher_transport::tls_over(tcp, host, tls).await?;
    let hello = ClientMsg::Hello {
        version: PROTOCOL_VERSION,
        peer: id.peer_id(),
    };
    conn.send(Frame::new(0, hello).encode()).await?;
    let nonce = match next(&mut conn).await? {
        ServerMsg::Challenge { nonce } => nonce,
        other => bail!("expected challenge, got {other:?}"),
    };
    let signed = [SESSION_AUTH_CONTEXT, nonce.as_slice()].concat();
    let signature = id.sign(&signed).to_bytes();
    conn.send(Frame::new(0, ClientMsg::Auth { signature }).encode())
        .await?;
    match next(&mut conn).await? {
        ServerMsg::Ready => Ok(conn),
        other => bail!("expected ready, got {other:?}"),
    }
}

/// Tries each resolved address in turn (`localhost` may resolve to `::1`
/// first), only those of `src`'s family when a source address is given.
async fn connect_tcp(host: &str, port: u16, src: Option<IpAddr>) -> Result<TcpStream> {
    let mut last = None;
    let usable = lookup_host((host, port))
        .await?
        .filter(|a| src.is_none_or(|ip| ip.is_ipv4() == a.is_ipv4()));
    for target in usable {
        let socket = if target.is_ipv4() {
            TcpSocket::new_v4()?
        } else {
            TcpSocket::new_v6()?
        };
        if let Some(ip) = src {
            socket.bind(SocketAddr::new(ip, 0))?;
        }
        match socket.connect(target).await {
            Ok(tcp) => return Ok(tcp),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map_or_else(|| anyhow!("no usable address for {host}"), Into::into))
}

async fn next(conn: &mut ClientConn) -> Result<ServerMsg> {
    let raw = conn.next().await.context("gateway closed")??;
    Ok(Frame::<ServerMsg>::decode(raw.freeze())?.msg)
}
