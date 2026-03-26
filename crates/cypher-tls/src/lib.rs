//! TLS configuration utilities for the p2p system.
//!
//! Provides:
//! - Self-signed certificate generation for development
//! - TLS server/client config builders
//! - Certificate loading from PEM files (for production)

pub mod cert;
pub mod config;

pub use cert::SelfSignedCert;
#[cfg(all(feature = "insecure-tls", debug_assertions))]
pub use config::make_client_config_insecure;
pub use config::{
    make_client_config, make_server_config, make_server_config_from_cert,
    make_server_config_from_pem,
};
