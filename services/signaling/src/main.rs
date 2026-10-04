#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cypher_server_kit::service_main(|c: &signaling::Config| c.metrics_addr, signaling::run).await
}
