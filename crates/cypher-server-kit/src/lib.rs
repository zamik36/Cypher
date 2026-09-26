//! Shared runtime plumbing for the Cypher services: configuration, NATS,
//! metrics, rate limiting, secrets on disk, tracing and shutdown.

pub mod metrics;
pub mod ratelimit;
pub mod secrets;

use serde::de::DeserializeOwned;

/// NATS header carrying the authenticated peer id (hex) on requests the
/// gateway forwards to signaling. Only infrastructure may publish to `sig.*`.
pub const PEER_HEADER: &str = "cy-peer";

pub const SIG_REQUEST_SUBJECT: &str = "sig.req";
pub const SIG_ONION_SUBJECT: &str = "sig.onion";
pub const SIG_QUEUE_GROUP: &str = "signaling";

/// Subject on which a connected peer's gateway accepts relayed frames.
pub fn peer_subject(peer_hex: &str) -> String {
    format!("peer.{peer_hex}")
}

/// Subject used to evict an older session of the same identity.
pub fn control_subject(peer_hex: &str) -> String {
    format!("ctl.{peer_hex}")
}

/// Loads `T` from an optional `config.toml` and `P2P_*` environment variables.
pub fn load_config<T: DeserializeOwned>() -> anyhow::Result<T> {
    Ok(config::Config::builder()
        .add_source(config::File::with_name("config").required(false))
        .add_source(config::Environment::with_prefix("P2P"))
        .build()?
        .try_deserialize()?)
}

/// NATS connection settings shared by every service config (`P2P_NATS_*`).
/// Production uses per-service users with least-privilege subject
/// permissions (`deploy/nats.conf`); a shared token is accepted for dev.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct NatsConfig {
    #[serde(default = "default_nats_url")]
    pub nats_url: String,
    pub nats_user: Option<String>,
    pub nats_password: Option<String>,
    pub nats_token: Option<String>,
}

fn default_nats_url() -> String {
    "nats://127.0.0.1:4222".into()
}

pub async fn connect_nats(config: &NatsConfig) -> anyhow::Result<async_nats::Client> {
    let non_empty = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());
    let options = match (
        non_empty(&config.nats_user),
        non_empty(&config.nats_password),
        non_empty(&config.nats_token),
    ) {
        (Some(user), Some(password), _) => {
            async_nats::ConnectOptions::with_user_and_password(user, password)
        }
        (_, _, Some(token)) => async_nats::ConnectOptions::with_token(token),
        _ => async_nats::ConnectOptions::new(),
    };
    Ok(options
        .retry_on_initial_connect()
        .connect(&config.nats_url)
        .await?)
}

/// Resolves on Ctrl-C, or SIGTERM on Unix.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

/// Text logs by default, JSON with `LOG_FORMAT=json`; level from `RUST_LOG`.
pub fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let json = std::env::var("LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("json"));
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

/// Hides credentials in URLs before they are logged.
pub fn redact_url(raw: &str) -> String {
    let Some(scheme_end) = raw.find("://").map(|i| i + 3) else {
        return raw.to_owned();
    };
    match raw[scheme_end..].find('@') {
        Some(at) => format!("{}***{}", &raw[..scheme_end], &raw[scheme_end + at..]),
        None => raw.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_credentials() {
        assert_eq!(
            redact_url("redis://:secret@redis:6379"),
            "redis://***@redis:6379"
        );
        assert_eq!(redact_url("nats://u:p@nats:4222"), "nats://***@nats:4222");
        assert_eq!(redact_url("nats://nats:4222"), "nats://nats:4222");
        assert_eq!(redact_url("plain"), "plain");
    }

    #[test]
    fn flattened_nats_config_keeps_numeric_env_parsing() {
        #[derive(serde::Deserialize)]
        struct Svc {
            #[serde(flatten)]
            nats: NatsConfig,
            limit: u64,
        }
        let cfg: Svc = config::Config::builder()
            .add_source(config::Environment::with_prefix("CYTEST").source(Some(
                std::collections::HashMap::from([
                    ("CYTEST_LIMIT".to_owned(), "42".to_owned()),
                    ("CYTEST_NATS_USER".to_owned(), "gateway".to_owned()),
                ]),
            )))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(cfg.limit, 42);
        assert_eq!(cfg.nats.nats_user.as_deref(), Some("gateway"));
        assert_eq!(cfg.nats.nats_url, "nats://127.0.0.1:4222");
    }

    #[test]
    fn subjects() {
        assert_eq!(peer_subject("ab"), "peer.ab");
        assert_eq!(control_subject("ab"), "ctl.ab");
    }
}
