//! Self-signed certificate generation for development.

use cypher_common::{Error, Result};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tracing::info;

/// A self-signed TLS certificate with its private key.
///
/// Generated at startup for development. In production, replace with
/// certificates from a CA (e.g. Let's Encrypt).
pub struct SelfSignedCert {
    pub cert_der: CertificateDer<'static>,
    pub key_der: PrivateKeyDer<'static>,
}

impl SelfSignedCert {
    /// Generate a new self-signed certificate for the given DNS names.
    ///
    /// # Example
    /// ```no_run
    /// # use cypher_tls::SelfSignedCert;
    /// let cert = SelfSignedCert::generate(&["localhost", "127.0.0.1"])?;
    /// # Ok::<(), cypher_common::Error>(())
    /// ```
    pub fn generate(names: &[&str]) -> Result<Self> {
        let mut params =
            CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
                .map_err(|e| Error::Transport(format!("cert params error: {e}")))?;

        let mut dn = DistinguishedName::new();
        dn.push(DnType::OrganizationName, "cypher-dev");
        dn.push(
            DnType::CommonName,
            names.first().copied().unwrap_or("localhost"),
        );
        params.distinguished_name = dn;

        let key_pair = KeyPair::generate()
            .map_err(|e| Error::Transport(format!("key generation error: {e}")))?;

        let cert = params
            .self_signed(&key_pair)
            .map_err(|e| Error::Transport(format!("self-sign error: {e}")))?;

        let cert_der = CertificateDer::from(cert.der().to_vec());
        let key_der = PrivateKeyDer::try_from(key_pair.serialize_der())
            .map_err(|e| Error::Transport(format!("private key error: {e}")))?;

        info!("Generated self-signed TLS certificate for: {:?}", names);

        Ok(Self { cert_der, key_der })
    }

    /// Load certificate and key from PEM files (for production deployments).
    pub fn from_pem_files(cert_path: &str, key_path: &str) -> Result<Self> {
        use rustls_pemfile::{certs, private_key};
        use std::fs::File;
        use std::io::BufReader;

        let cert_file = File::open(cert_path)
            .map_err(|e| Error::Transport(format!("cannot open cert file {cert_path}: {e}")))?;
        let key_file = File::open(key_path)
            .map_err(|e| Error::Transport(format!("cannot open key file {key_path}: {e}")))?;

        let cert_der = certs(&mut BufReader::new(cert_file))
            .next()
            .ok_or_else(|| Error::Transport("no certificate in PEM file".into()))?
            .map_err(|e| Error::Transport(format!("cert PEM parse error: {e}")))?;

        let key_der = private_key(&mut BufReader::new(key_file))
            .map_err(|e| Error::Transport(format!("key PEM parse error: {e}")))?
            .ok_or_else(|| Error::Transport("no private key in PEM file".into()))?;

        Ok(Self { cert_der, key_der })
    }
}
