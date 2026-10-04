#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cypher_server_kit::service_main(|c: &relay::Config| c.metrics_addr, relay::run).await
}
