//! The user journey against a deployed stack. Needs
//! `CYPHER_LIVE_GATEWAY=host:port` and `CYPHER_LIVE_CA=<pem bundle pinning
//! gateway and relay>`; skipped otherwise.

use e2e::Target;

fn live_target() -> Option<Target> {
    let gateway_addr = std::env::var("CYPHER_LIVE_GATEWAY").ok()?;
    let pem = std::fs::read_to_string(std::env::var("CYPHER_LIVE_CA").ok()?).ok()?;
    Some(Target {
        gateway_addr,
        tls: cypher_tls::make_client_config_with_pem(&pem).ok()?,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn user_journey_against_live_stack() {
    let Some(target) = live_target() else {
        eprintln!("CYPHER_LIVE_* not set; skipping");
        return;
    };
    e2e::journey(&target).await;
}
