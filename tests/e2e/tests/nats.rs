//! The NATS permissions in `deploy/nats.conf`, against a live server with
//! that file. Needs `CYPHER_TEST_NATS` and the service passwords; skipped
//! otherwise.

use std::time::Duration;

use cypher_server_kit::{NatsConfig, connect_nats};
use futures::StreamExt as _;

fn user(name: &str) -> Option<NatsConfig> {
    Some(NatsConfig {
        url: std::env::var("CYPHER_TEST_NATS").ok()?,
        user: Some(name.to_owned()),
        password: std::env::var(format!("{}_NATS_PASSWORD", name.to_uppercase())).ok(),
        token: None,
    })
}

/// A compromised relay must not read what signaling answers the gateway.
#[tokio::test]
async fn a_service_cannot_read_replies_meant_for_another() {
    let (Some(gateway), Some(relay)) = (user("gateway"), user("relay")) else {
        eprintln!("CYPHER_TEST_NATS not set; skipping");
        return;
    };
    let gateway = connect_nats(&gateway).await.unwrap();
    let relay = connect_nats(&relay).await.unwrap();
    // Where signaling's replies to the gateway actually go.
    let subject = gateway.new_inbox();
    assert!(subject.starts_with("_INBOX_gateway."), "{subject}");

    let mut own = gateway.subscribe(subject.clone()).await.unwrap();
    // The server refuses this subscription; the client sees no messages.
    let mut foreign = relay.subscribe(subject.clone()).await.unwrap();
    relay.flush().await.unwrap();
    gateway.flush().await.unwrap();

    gateway.publish(subject, "reply".into()).await.unwrap();
    gateway.flush().await.unwrap();
    let delivered = tokio::time::timeout(Duration::from_secs(2), own.next()).await;
    assert!(
        delivered.is_ok_and(|m| m.is_some()),
        "the gateway gets its own replies"
    );
    let leaked = tokio::time::timeout(Duration::from_millis(500), foreign.next()).await;
    assert!(
        !leaked.is_ok_and(|m| m.is_some()),
        "the relay must not see replies meant for the gateway"
    );
}
