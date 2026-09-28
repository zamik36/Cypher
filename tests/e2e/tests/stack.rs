//! The user journey against gateway, signaling and relay in this process.
//! Needs `CYPHER_TEST_REDIS` and `CYPHER_TEST_NATS`; skipped otherwise.

use e2e::Stack;

#[tokio::test(flavor = "multi_thread")]
async fn user_journey_against_in_process_stack() {
    cypher_server_kit::init_tracing();
    let Some(stack) = Stack::from_env().await else {
        eprintln!("CYPHER_TEST_REDIS / CYPHER_TEST_NATS not set; skipping");
        return;
    };
    e2e::journey(stack.target()).await;
    stack.stop().await;
}
