//! TLS configuration for Cypher services and native clients.

#[cfg(feature = "server")]
pub mod cert;
pub mod config;
mod error;
#[cfg(feature = "server")]
mod reload;
#[cfg(all(test, feature = "server"))]
mod testdata;

#[cfg(feature = "server")]
pub use cert::SelfSignedCert;
#[cfg(feature = "server")]
pub use config::{load_server_config, make_server_config_from_cert};
pub use config::{make_client_config, make_client_config_with_pem};
