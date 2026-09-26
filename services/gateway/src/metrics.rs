use cypher_server_kit::metrics::{counter, gauge};
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
    pub fn register() -> Self {
        Self {
            connections: gauge("gateway_connections", "Open client connections"),
            frames_in: counter("gateway_frames_in_total", "Frames received from clients"),
            bytes_in: counter("gateway_bytes_in_total", "Bytes received from clients"),
            delivered_local: counter(
                "gateway_delivered_local_total",
                "Relays delivered on this node",
            ),
            delivered_remote: counter(
                "gateway_delivered_remote_total",
                "Relays delivered via other nodes",
            ),
            busy: counter(
                "gateway_busy_total",
                "Deliveries refused by a full client queue",
            ),
            rate_limited: counter(
                "gateway_rate_limited_total",
                "Frames dropped by rate limiting",
            ),
            auth_failures: counter("gateway_auth_failures_total", "Failed authentications"),
            rejected: counter("gateway_rejected_total", "Connections rejected at capacity"),
        }
    }
}
