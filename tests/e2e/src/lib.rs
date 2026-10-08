//! Full-stack tests: the user journey every client build must pass, run
//! against the three services in-process ([`Stack`]) or a deployed stack.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a broken step must fail the test loudly"
)]

mod journey;
mod stack;

pub use journey::{Target, linking, run as journey};
pub use stack::Stack;
