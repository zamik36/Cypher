use std::net::IpAddr;
use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "load-test", about = "Cypher gateway load generator")]
pub(crate) struct Args {
    /// Client connections, rounded up to an even number: half send, half receive.
    #[arg(long, default_value_t = 100)]
    pub connections: usize,
    /// Seconds over which the connections are opened.
    #[arg(long, default_value_t = 5)]
    pub ramp: u64,
    /// Seconds of steady load once the ramp is over.
    #[arg(long, default_value_t = 30)]
    pub duration: u64,
    #[arg(long, default_value = "localhost:9100")]
    pub gateway_addr: String,
    /// Connect the receiving side of each pair here to exercise cross-node
    /// delivery through NATS.
    #[arg(long)]
    pub peer_gateway_addr: Option<String>,
    /// PEM certificate to pin (development gateways).
    #[arg(long)]
    pub ca_cert: Option<PathBuf>,
    /// Local addresses to spread connections over; one address runs out of
    /// ephemeral ports at about 28k connections to the same gateway.
    #[arg(long, value_delimiter = ',')]
    pub src_ips: Vec<IpAddr>,
    /// Relayed messages per second per sending client.
    #[arg(long, default_value_t = 5)]
    pub msg_rate: u32,
    #[arg(long, default_value_t = 256)]
    pub payload: usize,
    /// Gateway metrics endpoint (`host:port`): its resident memory before and
    /// after the ramp gives the memory cost of one connection.
    #[arg(long)]
    pub metrics_addr: Option<String>,
    /// Print the report as JSON.
    #[arg(long)]
    pub json: bool,
    /// Fail when more connections or pairs than this fail.
    #[arg(long)]
    pub max_errors: Option<u64>,
    /// Fail when the relay round trip's 99th percentile exceeds this.
    #[arg(long)]
    pub assert_p99_ms: Option<u64>,
    /// Fail when one connection costs the gateway more memory than this.
    #[arg(long)]
    pub assert_max_bytes_per_conn: Option<u64>,
    /// Fail unless, once every client has left, the gateway runs at most this
    /// many more tasks than before the run: a leaked connection task.
    #[arg(long)]
    pub assert_tasks_return: Option<u64>,
}
