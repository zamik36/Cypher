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
    // Once contacts have told each other their tokens, messages go sealed:
    // the gateway hands them over without knowing who sends.
    assert!(stack.counter("gateway_sealed_delivered_total").await > 0);
    assert_eq!(stack.counter("gateway_sealed_refused_total").await, 0);
    // Every one was answered: a gateway that cannot reply to signaling
    // (NATS rights) delivers, and the sender then sends again by name.
    assert_eq!(stack.counter("signaling_sealed_unavailable_total").await, 0);
    stack.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn linking_a_device_against_in_process_stack() {
    let Some(stack) = Stack::from_env().await else {
        eprintln!("CYPHER_TEST_REDIS / CYPHER_TEST_NATS not set; skipping");
        return;
    };
    e2e::linking(stack.target()).await;
    stack.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restoring_from_the_phrase_against_in_process_stack() {
    let Some(stack) = Stack::from_env().await else {
        eprintln!("CYPHER_TEST_REDIS / CYPHER_TEST_NATS not set; skipping");
        return;
    };
    e2e::restoring(stack.target()).await;
    stack.stop().await;
}
