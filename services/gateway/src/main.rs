#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config: gateway::Config = cypher_server_kit::load_config()?;
    if std::env::args().nth(1).as_deref() == Some("health") {
        return cypher_server_kit::metrics::probe_ready(config.metrics_addr);
    }
    cypher_server_kit::init_tracing();
    gateway::run(config, cypher_server_kit::shutdown_token()).await
}
