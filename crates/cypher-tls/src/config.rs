//! TLS `ServerConfig` and `ClientConfig` builders.

use std::sync::Arc;

use cypher_common::{Error, Result};
use rustls::crypto::ring::default_provider;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use tracing::debug;

use crate::cert::SelfSignedCert;

/// Ensure a process-level `CryptoProvider` is installed (idempotent).
fn ensure_crypto_provider() {
    let _ = default_provider().install_default();
}

/// Build a TLS [`ServerConfig`] from a [`SelfSignedCert`].
pub fn make_server_config_from_cert(cert: SelfSignedCert) -> Result<Arc<ServerConfig>> {
    ensure_crypto_provider();
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert_der], cert.key_der)
        .map_err(|e| Error::Transport(format!("TLS server config error: {e}")))?;

    debug!("TLS server config created");
    Ok(Arc::new(config))
}

/// Build a TLS [`ServerConfig`] from PEM certificate and key files on disk.
///
/// Suitable for production use with CA-signed certificates (e.g. from Let's Encrypt).
pub fn make_server_config_from_pem(cert_path: &str, key_path: &str) -> Result<Arc<ServerConfig>> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use std::io::BufReader;

    ensure_crypto_provider();

    let cert_file = std::fs::File::open(cert_path)
        .map_err(|e| Error::Transport(format!("failed to open cert {cert_path}: {e}")))?;
    let key_file = std::fs::File::open(key_path)
        .map_err(|e| Error::Transport(format!("failed to open key {key_path}: {e}")))?;

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut BufReader::new(cert_file))
        .filter_map(|r| match r {
            Ok(cert) => Some(cert),
            Err(e) => {
                tracing::warn!("skipping invalid certificate entry in {}: {}", cert_path, e);
                None
            }
        })
        .collect();
    if certs.is_empty() {
        return Err(Error::Transport("no certificates found in PEM file".into()));
    }

    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut BufReader::new(key_file))
        .map_err(|e| Error::Transport(format!("failed to read private key: {e}")))?
        .ok_or_else(|| Error::Transport("no private key found in PEM file".into()))?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| Error::Transport(format!("TLS server config error: {e}")))?;

    debug!("TLS server config created from PEM files");
    Ok(Arc::new(config))
}

/// Load TLS server config from PEM files, retrying if files are not yet available.
///
/// Useful when certificates are provided by another container (e.g. Caddy with
/// auto-HTTPS) that may not have written them to disk at the moment this
/// service starts.
pub async fn load_pem_with_retry(
    cert_path: &str,
    key_path: &str,
    max_attempts: u32,
    interval: std::time::Duration,
) -> Result<Arc<ServerConfig>> {
    let mut last_err = None;

    for attempt in 1..=max_attempts {
        // Fail fast on permanent errors (wrong permissions, not a file, etc.).
        // Only retry when the file simply does not exist yet (ENOENT).
        for path in [cert_path, key_path] {
            match std::fs::metadata(path) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // File not yet written — transient, worth retrying.
                }
                Err(e) => {
                    return Err(Error::Transport(format!(
                        "permanent I/O error for {path}: {e}"
                    )));
                }
            }
        }

        match make_server_config_from_pem(cert_path, key_path) {
            Ok(config) => {
                tracing::info!(
                    attempt,
                    max_attempts,
                    "TLS certificates loaded from PEM files"
                );
                return Ok(config);
            }
            Err(e) if attempt < max_attempts => {
                tracing::warn!(
                    attempt,
                    max_attempts,
                    %e,
                    "TLS cert not ready, retrying in {:?}…",
                    interval
                );
                last_err = Some(e);
                tokio::time::sleep(interval).await;
            }
            Err(e) => {
                return Err(Error::Transport(format!(
                    "failed to load TLS certs after {max_attempts} attempts: {e}"
                )));
            }
        }
    }

    Err(last_err.unwrap_or_else(|| Error::Transport("no retry attempts made".into())))
}

/// Build a TLS [`ServerConfig`] using a freshly generated self-signed certificate.
///
/// Intended for development and testing. In production, use
/// [`make_server_config_from_cert`] with certificates loaded from disk.
pub fn make_server_config(hostnames: &[&str]) -> Result<Arc<ServerConfig>> {
    let cert = SelfSignedCert::generate(hostnames)?;
    make_server_config_from_cert(cert)
}

/// Build a TLS [`ClientConfig`] that accepts any certificate issued by a
/// specific self-signed CA cert (for dev/test scenarios).
///
/// In production, use [`make_client_config`] which validates against the
/// system's trusted roots.
pub fn make_client_config_with_cert(
    ca_cert: rustls::pki_types::CertificateDer<'static>,
) -> Result<Arc<ClientConfig>> {
    ensure_crypto_provider();
    let mut roots = RootCertStore::empty();
    roots
        .add(ca_cert)
        .map_err(|e| Error::Transport(format!("failed to add CA cert: {e}")))?;

    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    Ok(Arc::new(config))
}

/// Build a TLS [`ClientConfig`] using both native OS certificates and the
/// Mozilla root store as a fallback.
///
/// Tries `rustls-native-certs` first (Android, Windows, macOS, Linux) so that
/// platform-trusted CAs are honoured, then adds `webpki-roots` as a baseline
/// to guarantee coverage when the OS store is empty or unavailable.
pub fn make_client_config() -> Arc<ClientConfig> {
    ensure_crypto_provider();
    let mut roots = RootCertStore::empty();

    // Native OS trust store (works on Android, Windows, macOS, Linux).
    let native = rustls_native_certs::load_native_certs();
    let count = native.certs.len();
    for cert in native.certs {
        let _ = roots.add(cert);
    }
    if count > 0 {
        debug!("loaded {} native CA certificates", count);
    }
    for e in native.errors {
        tracing::warn!("native cert load error: {e}");
    }

    // Always include webpki-roots as baseline fallback.
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    Arc::new(config)
}

/// Build a TLS [`ClientConfig`] that accepts **any** server certificate.
///
/// **Development/testing only** — disables all certificate verification.
/// Use [`make_client_config`] in production.
///
/// Compile-time guarded: only available with `insecure-tls` feature AND debug builds.
#[cfg(feature = "insecure-tls")]
#[cfg(debug_assertions)]
pub fn make_client_config_insecure() -> Arc<ClientConfig> {
    ensure_crypto_provider();

    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoCertVerifier))
        .with_no_client_auth();

    Arc::new(config)
}

/// Certificate verifier that accepts everything (dev only).
#[cfg(feature = "insecure-tls")]
#[cfg(debug_assertions)]
#[derive(Debug)]
struct NoCertVerifier;

#[cfg(feature = "insecure-tls")]
#[cfg(debug_assertions)]
impl rustls::client::danger::ServerCertVerifier for NoCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
