#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // For dependencies that build their own TLS configs (NATS over TLS);
    // ours carry their provider. Nothing can have installed one yet.
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    cypher_server_kit::service_main(|c: &gateway::Config| c.metrics_addr, gateway::run).await
}
