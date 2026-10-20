//! TLS `ServerConfig` and `ClientConfig` builders.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cypher_types::{Error, Result};
use rustls::crypto::CryptoProvider;
use rustls::crypto::ring::default_provider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, ConfigBuilder, RootCertStore, ServerConfig, WantsVerifier};
use tracing::debug;

use crate::cert::SelfSignedCert;
use crate::reload::{PemCert, pem_reason};

/// How long a service waits for certificates another container writes
/// (e.g. Caddy with auto-HTTPS) before giving up.
const PEM_ATTEMPTS: u32 = 30;
const PEM_RETRY: Duration = Duration::from_secs(2);

/// The crypto every config here is built with. The process-wide default
/// is left to the binary: a library setting it would race whichever other
/// provider the application or another dependency installs.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(default_provider())
}

fn server_builder() -> Result<ConfigBuilder<ServerConfig, WantsVerifier>> {
    ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| transport("TLS server config error", e))
}

fn client_builder() -> Result<ConfigBuilder<ClientConfig, WantsVerifier>> {
    ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| transport("TLS client config error", e))
}

fn transport(context: &str, e: impl std::fmt::Display) -> Error {
    Error::Transport(format!("{context}: {e}"))
}

/// Build a TLS [`ServerConfig`] from a [`SelfSignedCert`].
pub fn make_server_config_from_cert(cert: SelfSignedCert) -> Result<Arc<ServerConfig>> {
    server_config(vec![cert.cert_der], cert.key_der)
}

fn server_config(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<Arc<ServerConfig>> {
    let config = server_builder()?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| transport("TLS server config error", e))?;
    debug!("TLS server config created");
    Ok(Arc::new(config))
}

/// Server config from CA-issued PEM files (e.g. Let's Encrypt). The
/// certificate is served through [`PemCert`], which picks up renewals.
fn server_config_from_pem(
    cert_path: &str,
    key_path: &str,
) -> Result<(Arc<ServerConfig>, Arc<PemCert>)> {
    let cert = Arc::new(PemCert::load(cert_path, key_path)?);
    let resolver: Arc<dyn rustls::server::ResolvesServerCert> = Arc::<PemCert>::clone(&cert);
    let config = server_builder()?
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    Ok((Arc::new(config), cert))
}

/// Loads PEM files, waiting while they do not exist yet; any other error
/// (unreadable, malformed) fails at once or after the last attempt.
async fn load_pem_with_retry(
    cert_path: &str,
    key_path: &str,
    attempts: u32,
    interval: Duration,
) -> Result<Arc<ServerConfig>> {
    let mut attempt = 1;
    loop {
        for path in [cert_path, key_path] {
            if let Err(e) = std::fs::metadata(path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                return Err(transport(&format!("cannot access {path}"), e));
            }
        }
        match server_config_from_pem(cert_path, key_path) {
            Ok((config, cert)) => {
                tracing::info!(attempt, "TLS certificates loaded from PEM files");
                PemCert::watch(&cert);
                return Ok(config);
            }
            Err(e) if attempt < attempts => {
                tracing::warn!(attempt, attempts, %e, "TLS certificates not ready, retrying");
                attempt += 1;
                tokio::time::sleep(interval).await;
            }
            Err(e) => {
                return Err(transport(
                    &format!("no TLS certificates after {attempts} attempts"),
                    e,
                ));
            }
        }
    }
}

/// Build a TLS [`ClientConfig`] using both native OS certificates and the
/// Mozilla root store as a fallback.
///
/// Tries `rustls-native-certs` first (Android, Windows, macOS, Linux) so that
/// platform-trusted CAs are honoured, then adds `webpki-roots` as a baseline
/// to guarantee coverage when the OS store is empty or unavailable.
#[expect(
    clippy::expect_used,
    reason = "ring's default provider supports every default protocol version"
)]
pub fn make_client_config() -> Arc<ClientConfig> {
    let builder = client_builder().expect("ring supports the default protocol versions");
    let mut roots = RootCertStore::empty();

    let native = rustls_native_certs::load_native_certs();
    let (added, _) = roots.add_parsable_certificates(native.certs);
    debug!("loaded {added} native CA certificates");
    for e in native.errors {
        tracing::warn!("native cert load error: {e}");
    }
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    Arc::new(builder.with_root_certificates(roots).with_no_client_auth())
}

/// Client config pinning every certificate in a PEM bundle.
pub fn make_client_config_with_pem(pem: &str) -> Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(pem.as_bytes()) {
        let cert = cert.map_err(|e| transport("invalid certificate PEM", pem_reason(&e)))?;
        roots
            .add(cert)
            .map_err(|e| transport("failed to add certificate", e))?;
    }
    if roots.is_empty() {
        return Err(Error::Transport("no certificate in PEM".into()));
    }
    Ok(Arc::new(
        client_builder()?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

/// Server config for a service: CA-issued PEM files in production, or a
/// fresh self-signed certificate in development whose PEM is written to
/// `dev_cert_out` so local clients can pin it.
pub async fn load_server_config(
    cert_path: Option<&str>,
    key_path: Option<&str>,
    dev_hostnames: &[&str],
    dev_cert_out: Option<&Path>,
) -> Result<Arc<ServerConfig>> {
    match (
        cert_path.filter(|p| !p.is_empty()),
        key_path.filter(|p| !p.is_empty()),
    ) {
        (Some(cert), Some(key)) => load_pem_with_retry(cert, key, PEM_ATTEMPTS, PEM_RETRY).await,
        (None, None) => {
            let cert = SelfSignedCert::generate(dev_hostnames)?;
            if let Some(out) = dev_cert_out {
                std::fs::write(out, &cert.cert_pem)?;
            }
            tracing::warn!("using a self-signed development certificate");
            make_server_config_from_cert(cert)
        }
        _ => Err(Error::Config(
            "tls_cert_path and tls_key_path must be set together".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdata;

    struct PemFiles {
        dir: tempfile::TempDir,
        cert_pem: String,
        key_pem: String,
    }

    impl PemFiles {
        fn new() -> Self {
            let key = rcgen::KeyPair::generate().unwrap();
            let cert = rcgen::CertificateParams::new(vec!["localhost".to_owned()])
                .unwrap()
                .self_signed(&key)
                .unwrap();
            Self {
                dir: tempfile::tempdir().unwrap(),
                cert_pem: cert.pem(),
                key_pem: key.serialize_pem(),
            }
        }

        fn path(&self, name: &str) -> String {
            self.dir.path().join(name).to_string_lossy().into_owned()
        }

        fn write(&self, name: &str, contents: &str) -> String {
            let path = self.path(name);
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    async fn load(cert: &str, key: &str) -> Result<Arc<ServerConfig>> {
        load_pem_with_retry(cert, key, 2, Duration::from_millis(1)).await
    }

    #[tokio::test]
    async fn dev_certificate_is_written_for_clients_to_pin() {
        let files = PemFiles::new();
        let out = files.dir.path().join("dev.pem");
        load_server_config(None, Some(""), &["localhost"], Some(&out))
            .await
            .unwrap();
        let pem = std::fs::read_to_string(&out).unwrap();
        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----"));
        make_client_config_with_pem(&pem).unwrap();
    }

    #[tokio::test]
    async fn half_configured_pem_paths_are_rejected() {
        let only_cert = load_server_config(Some("cert.pem"), None, &[], None).await;
        assert!(matches!(only_cert, Err(Error::Config(_))));
        let only_key = load_server_config(Some(""), Some("key.pem"), &[], None).await;
        assert!(matches!(only_key, Err(Error::Config(_))));
    }

    /// Runs a TLS handshake in memory; returns the certificate chain the
    /// client was shown.
    fn handshake(
        server: Arc<ServerConfig>,
        client: Arc<ClientConfig>,
    ) -> Vec<CertificateDer<'static>> {
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let mut client = rustls::ClientConnection::new(client, name).unwrap();
        let mut server = rustls::ServerConnection::new(server).unwrap();
        while client.is_handshaking() || server.is_handshaking() {
            let mut wire = Vec::new();
            client.write_tls(&mut wire).unwrap();
            server.read_tls(&mut wire.as_slice()).unwrap();
            server.process_new_packets().unwrap();
            wire.clear();
            server.write_tls(&mut wire).unwrap();
            client.read_tls(&mut wire.as_slice()).unwrap();
            client.process_new_packets().unwrap();
        }
        client.peer_certificates().unwrap().to_vec()
    }

    #[tokio::test]
    async fn ca_issued_pem_files_are_served() {
        let files = PemFiles::new();
        let cert = files.write("cert.pem", &files.cert_pem);
        let key = files.write("key.pem", &files.key_pem);
        let server = load_server_config(Some(&cert), Some(&key), &[], None)
            .await
            .unwrap();
        let client = make_client_config_with_pem(&files.cert_pem).unwrap();
        let shown = handshake(server, client);
        let expected = CertificateDer::from_pem_slice(files.cert_pem.as_bytes()).unwrap();
        assert_eq!(shown.first(), Some(&expected));
    }

    #[tokio::test]
    async fn malformed_pem_files_fail_with_the_reason() {
        let files = PemFiles::new();
        let cert = files.write("cert.pem", &files.cert_pem);
        let key = files.write("key.pem", &files.key_pem);
        let empty = files.write("empty.pem", "");
        let broken = files.write(
            "broken.pem",
            "-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n",
        );
        let reason = |r: Result<Arc<ServerConfig>>| r.unwrap_err().to_string();
        assert!(reason(load(&empty, &key).await).contains("no certificate"));
        assert!(reason(load(&broken, &key).await).contains("invalid certificate"));
        assert!(reason(load(&cert, &empty).await).contains("no private key"));
        assert!(reason(load(&cert, &cert).await).contains("no private key"));
        assert!(reason(load(&key, &cert).await).contains("no certificate"));
        let bad_key = files.write(
            "bad-key.pem",
            "-----BEGIN PRIVATE KEY-----\n!!!\n-----END PRIVATE KEY-----\n",
        );
        assert!(reason(load(&cert, &bad_key).await).contains("invalid private key"));
        let cut = files.write(
            "cut.pem",
            "-----BEGIN CERTIFICATE-----
MIIB
",
        );
        let cut = reason(load(&cut, &key).await);
        assert!(cut.contains("missing -----END CERTIFICATE-----"), "{cut}");
    }

    /// Operators bring keys in every PEM form: PKCS#8 (the rcgen pairs
    /// above), SEC1 (`EC PRIVATE KEY`) and PKCS#1 (`RSA PRIVATE KEY`).
    #[tokio::test]
    async fn sec1_and_pkcs1_keys_are_served() {
        let files = PemFiles::new();
        for (name, (cert_pem, key_pem)) in
            [("sec1", testdata::sec1()), ("pkcs1", testdata::pkcs1())]
        {
            let cert = files.write(&format!("{name}-cert.pem"), &cert_pem);
            let key = files.write(&format!("{name}-key.pem"), &key_pem);
            let server = load(&cert, &key).await.unwrap();
            let client = make_client_config_with_pem(&cert_pem).unwrap();
            assert_eq!(handshake(server, client).len(), 1, "{name}");
        }
    }

    #[tokio::test]
    async fn the_whole_chain_in_the_certificate_file_is_served() {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::from_params(&ca_params, &ca_key);
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf = rcgen::CertificateParams::new(vec!["localhost".to_owned()])
            .unwrap()
            .signed_by(&leaf_key, &issuer)
            .unwrap();

        let files = PemFiles::new();
        let cert = files.write("chain.pem", &[leaf.pem(), ca.pem()].concat());
        let key = files.write("key.pem", &leaf_key.serialize_pem());
        let server = load(&cert, &key).await.unwrap();
        let client = make_client_config_with_pem(&ca.pem()).unwrap();
        let shown = handshake(server, client);
        assert_eq!(shown.len(), 2);
        assert_eq!(shown.first().map(AsRef::as_ref), Some(leaf.der().as_ref()));
        assert_eq!(shown.get(1).map(AsRef::as_ref), Some(ca.der().as_ref()));
    }

    #[tokio::test]
    async fn one_file_may_hold_both_the_key_and_the_certificate() {
        let files = PemFiles::new();
        let pem = [files.key_pem.as_str(), files.cert_pem.as_str()].concat();
        let both = files.write("both.pem", &pem);
        let server = load(&both, &both).await.unwrap();
        let client = make_client_config_with_pem(&pem).unwrap();
        assert_eq!(handshake(server, client).len(), 1);
    }

    #[tokio::test]
    async fn missing_files_are_awaited_then_reported() {
        let files = PemFiles::new();
        let (cert, key) = (files.path("cert.pem"), files.path("key.pem"));
        let err = load(&cert, &key).await.unwrap_err().to_string();
        assert!(err.contains("after 2 attempts"), "{err}");

        let writer = {
            let (cert, key) = (cert.clone(), key.clone());
            let (cert_pem, key_pem) = (files.cert_pem.clone(), files.key_pem.clone());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                std::fs::write(key, key_pem).unwrap();
                std::fs::write(cert, cert_pem).unwrap();
            })
        };
        load_pem_with_retry(&cert, &key, 100, Duration::from_millis(10))
            .await
            .unwrap();
        writer.await.unwrap();
    }

    #[test]
    fn pinned_bundle_needs_valid_certificates() {
        let files = PemFiles::new();
        let bundle = [files.cert_pem.as_str(), PemFiles::new().cert_pem.as_str()].concat();
        make_client_config_with_pem(&bundle).unwrap();
        make_client_config_with_pem("").unwrap_err();
        make_client_config_with_pem(&files.key_pem).unwrap_err();
        make_client_config_with_pem(
            "-----BEGIN CERTIFICATE-----
!!!
-----END CERTIFICATE-----
",
        )
        .unwrap_err();
    }

    #[test]
    fn system_client_config_builds() {
        make_client_config();
    }

    /// Configs carry their own provider: building them installs no
    /// process-wide default, which is the binary's to choose.
    #[test]
    fn configs_leave_the_process_provider_alone() {
        let pem = SelfSignedCert::generate(&["localhost"]).unwrap();
        make_client_config_with_pem(&pem.cert_pem).unwrap();
        make_client_config();
        make_server_config_from_cert(pem).unwrap();
        assert!(CryptoProvider::get_default().is_none());
    }
}
