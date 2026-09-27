#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cypher_server_kit::init_tracing();
    let config: gateway::Config = cypher_server_kit::load_config()?;
    gateway::run(config, cypher_server_kit::shutdown_token()).await
}
