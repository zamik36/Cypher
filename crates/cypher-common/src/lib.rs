pub mod config;
pub mod error;
pub mod metrics;
pub mod ratelimit;
pub mod types;

pub use config::AppConfig;
pub use error::{Error, Result};
pub use types::*;

pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    fmt().with_env_filter(filter).with_target(true).init();
}
