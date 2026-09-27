//! Self-signed certificate generation for development.

use cypher_types::{Error, Result};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tracing::info;

/// A self-signed TLS certificate with its private key.
///
/// Generated at startup for development. In production, replace with
/// certificates from a CA (e.g. Let's Encrypt).
pub struct SelfSignedCert {
    pub cert_der: CertificateDer<'static>,
    pub cert_pem: String,
    pub key_der: PrivateKeyDer<'static>,
}

impl SelfSignedCert {
    /// Generate a new self-signed certificate for the given DNS names.
    ///
    /// # Example
    /// ```no_run
    /// # use cypher_tls::SelfSignedCert;
    /// let cert = SelfSignedCert::generate(&["localhost", "127.0.0.1"])?;
    /// # Ok::<(), cypher_types::Error>(())
    /// ```
    pub fn generate(names: &[&str]) -> Result<Self> {
        let mut params =
            CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>())
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
        let cert_pem = cert.pem();
        let key_der = PrivateKeyDer::try_from(key_pair.serialize_der())
            .map_err(|e| Error::Transport(format!("private key error: {e}")))?;

        info!("Generated self-signed TLS certificate for: {:?}", names);

        Ok(Self {
            cert_der,
            cert_pem,
            key_der,
        })
    }
}
