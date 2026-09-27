use cypher_server_kit::metrics::Metrics as Registry;
use prometheus::{IntCounter, IntGauge};

pub struct Metrics {
    pub connections: IntGauge,
    pub frames_in: IntCounter,
    pub bytes_in: IntCounter,
    pub delivered_local: IntCounter,
    pub delivered_remote: IntCounter,
    pub busy: IntCounter,
    pub rate_limited: IntCounter,
    pub auth_failures: IntCounter,
    pub rejected: IntCounter,
}

impl Metrics {
    pub fn register(r: &Registry) -> anyhow::Result<Self> {
        Ok(Self {
            connections: r.gauge("gateway_connections", "Open client connections")?,
            frames_in: r.counter("gateway_frames_in_total", "Frames received from clients")?,
            bytes_in: r.counter("gateway_bytes_in_total", "Bytes received from clients")?,
            delivered_local: r.counter(
                "gateway_delivered_local_total",
                "Relays delivered on this node",
            )?,
            delivered_remote: r.counter(
                "gateway_delivered_remote_total",
                "Relays delivered via other nodes",
            )?,
            busy: r.counter(
                "gateway_busy_total",
                "Deliveries refused by a full client queue",
            )?,
            rate_limited: r.counter(
                "gateway_rate_limited_total",
                "Frames dropped by rate limiting",
            )?,
            auth_failures: r.counter("gateway_auth_failures_total", "Failed authentications")?,
            rejected: r.counter("gateway_rejected_total", "Connections rejected at capacity")?,
        })
    }
}
