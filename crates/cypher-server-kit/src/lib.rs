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

/// Subject on which a connected peer's gateway accepts relayed frames. An
/// empty message on it evicts the session holding it: relayed frames are
/// never empty, and one subscription per connection halves NATS state.
pub fn peer_subject(peer_hex: &str) -> String {
    format!("peer.{peer_hex}")
}

/// Everything a service's `main` does: `<service> health` asks a running
/// instance on `metrics_addr` whether it is ready (the container
/// healthcheck); otherwise the service runs until a shutdown signal.
pub async fn service_main<C, F>(
    metrics_addr: impl FnOnce(&C) -> std::net::SocketAddr,
    run: impl FnOnce(C, tokio_util::sync::CancellationToken) -> F,
) -> anyhow::Result<()>
where
    C: DeserializeOwned,
    F: Future<Output = anyhow::Result<()>>,
{
    let config: C = load_config()?;
    if std::env::args().nth(1).as_deref() == Some("health") {
        return metrics::probe_ready(metrics_addr(&config));
    }
    init_tracing();
    run(config, shutdown_token()).await
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
    #[serde(rename = "nats_url", default = "default_nats_url")]
    pub url: String,
    #[serde(rename = "nats_user")]
    pub user: Option<String>,
    #[serde(rename = "nats_password")]
    pub password: Option<String>,
    #[serde(rename = "nats_token")]
    pub token: Option<String>,
}

fn default_nats_url() -> String {
    "nats://127.0.0.1:4222".into()
}

/// How a service authenticates to NATS.
#[derive(Debug, PartialEq, Eq)]
enum NatsAuth<'a> {
    UserPassword(&'a str, &'a str),
    Token(&'a str),
    Anonymous,
}

impl NatsConfig {
    /// A user with a password wins over a token; empty values (unset
    /// variables in compose files) count as absent.
    fn auth(&self) -> NatsAuth<'_> {
        fn set(v: Option<&String>) -> Option<&str> {
            v.map(String::as_str).filter(|s| !s.is_empty())
        }
        match (
            set(self.user.as_ref()),
            set(self.password.as_ref()),
            set(self.token.as_ref()),
        ) {
            (Some(user), Some(password), _) => NatsAuth::UserPassword(user, password),
            (_, _, Some(token)) => NatsAuth::Token(token),
            _ => NatsAuth::Anonymous,
        }
    }
}

/// Connects to NATS. A named user gets replies on its own `_INBOX_<user>`
/// prefix, which `deploy/nats.conf` lets only that user subscribe to: one
/// service cannot read replies meant for another.
pub async fn connect_nats(config: &NatsConfig) -> anyhow::Result<async_nats::Client> {
    let options = match config.auth() {
        NatsAuth::UserPassword(user, password) => {
            async_nats::ConnectOptions::with_user_and_password(user.to_owned(), password.to_owned())
                .custom_inbox_prefix(format!("_INBOX_{user}"))
        }
        NatsAuth::Token(token) => async_nats::ConnectOptions::with_token(token.to_owned()),
        NatsAuth::Anonymous => async_nats::ConnectOptions::new(),
    };
    Ok(options
        .retry_on_initial_connect()
        .connect(&config.url)
        .await?)
}

/// A token cancelled on Ctrl-C or SIGTERM, for a service's `run`.
pub fn shutdown_token() -> tokio_util::sync::CancellationToken {
    let token = tokio_util::sync::CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("shutdown signal received");
        cancel.cancel();
    });
    token
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
/// Installs the subscriber once; later calls do nothing.
/// With the `console` feature the runtime is also served to tokio-console
/// on `127.0.0.1:6669` (`TOKIO_CONSOLE_BIND`); `RUST_LOG` filters only logs.
pub fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    use tracing_subscriber::{EnvFilter, Layer as _, fmt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let json = std::env::var("LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("json"));
    let logs = if json {
        fmt::layer().json().with_target(true).boxed()
    } else {
        fmt::layer().with_target(true).boxed()
    };
    let subscriber = tracing_subscriber::registry().with(logs.with_filter(filter));
    #[cfg(all(feature = "console", tokio_unstable))]
    let subscriber = subscriber.with(console_subscriber::spawn());
    // Once per process; a second call (tests) keeps the first subscriber.
    let _ = subscriber.try_init();
    // console-subscriber panics on a runtime built without the cfg flag, so
    // `--all-features` builds (CI, coverage) run without the console.
    #[cfg(all(feature = "console", not(tokio_unstable)))]
    tracing::warn!("tokio-console needs RUSTFLAGS=\"--cfg tokio_unstable\"; not started");
}

/// Hides credentials in URLs before they are logged.
pub fn redact_url(raw: &str) -> String {
    let Some((scheme, rest)) = raw.split_once("://") else {
        return raw.to_owned();
    };
    match rest.rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://***@{host}"),
        None => raw.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Outside `health`, a service's main loads its config and runs it with
    /// a shutdown token.
    #[tokio::test]
    async fn service_main_runs_the_service_with_its_config() {
        #[derive(serde::Deserialize)]
        struct Svc {
            #[serde(default)]
            limit: u32,
        }
        let ran = std::sync::atomic::AtomicBool::new(false);
        service_main(
            |_: &Svc| std::net::SocketAddr::from(([127, 0, 0, 1], 1)),
            |svc: Svc, shutdown| {
                assert_eq!(svc.limit, 0);
                assert!(!shutdown.is_cancelled());
                ran.store(true, std::sync::atomic::Ordering::SeqCst);
                async { Ok(()) }
            },
        )
        .await
        .unwrap();
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    }

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
        assert_eq!(cfg.nats.user.as_deref(), Some("gateway"));
        assert_eq!(cfg.nats.url, "nats://127.0.0.1:4222");
    }

    #[test]
    fn nats_auth_precedence() {
        let config = |user: &str, password: &str, token: &str| {
            let opt = |v: &str| (!v.is_empty()).then(|| v.to_owned());
            NatsConfig {
                url: default_nats_url(),
                user: opt(user),
                password: opt(password),
                token: opt(token),
            }
        };
        let both = config("gateway", "pw", "tok");
        assert_eq!(both.auth(), NatsAuth::UserPassword("gateway", "pw"));
        assert_eq!(config("gateway", "", "tok").auth(), NatsAuth::Token("tok"));
        assert_eq!(config("", "pw", "").auth(), NatsAuth::Anonymous);
        let empty = NatsConfig {
            user: Some(String::new()),
            password: Some(String::new()),
            ..config("", "", "")
        };
        assert_eq!(
            empty.auth(),
            NatsAuth::Anonymous,
            "empty env values are unset"
        );
    }

    #[test]
    fn subjects() {
        assert_eq!(peer_subject("ab"), "peer.ab");
    }
}
