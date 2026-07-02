pub mod config;
pub mod error;
pub mod metrics;
pub mod ratelimit;
pub mod types;

pub use config::AppConfig;
pub use error::{Error, Result};
pub use types::*;

/// Domain-separation prefix signed together with the gateway's `server_nonce`
/// during SESSION_INIT proof-of-possession. Shared by clients and the gateway so
/// a signature can never be repurposed for a different context.
pub const SESSION_AUTH_CONTEXT: &[u8] = b"cypher-session-auth-v1";

pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let json = std::env::var("LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    if json {
        fmt()
            .json()
            .with_env_filter(filter)
            .with_target(true)
            .init();
    } else {
        fmt().with_env_filter(filter).with_target(true).init();
    }
}
