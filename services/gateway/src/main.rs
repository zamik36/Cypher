#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cypher_server_kit::service_main(|c: &gateway::Config| c.metrics_addr, gateway::run).await
}
