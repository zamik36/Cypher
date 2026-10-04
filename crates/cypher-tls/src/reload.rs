//! A server certificate read from PEM files and re-read when they change,
//! so a renewal (Let's Encrypt replaces it every ~60 days) takes effect
//! without restarting the service.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, SystemTime};

use cypher_types::{Error, Result};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

/// How often the files are checked for a newer modification time.
const CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// Modification times of the certificate and key files at the last load.
type Stamp = (Option<SystemTime>, Option<SystemTime>);

#[derive(Debug)]
pub(crate) struct PemCert {
    cert_path: PathBuf,
    key_path: PathBuf,
    current: RwLock<Arc<CertifiedKey>>,
    seen: Mutex<Stamp>,
}

impl PemCert {
    pub(crate) fn load(cert_path: &str, key_path: &str) -> Result<Self> {
        let (cert_path, key_path) = (PathBuf::from(cert_path), PathBuf::from(key_path));
        let seen = stamp(&cert_path, &key_path);
        let current = read(&cert_path, &key_path)?;
        Ok(Self {
            cert_path,
            key_path,
            current: RwLock::new(Arc::new(current)),
            seen: Mutex::new(seen),
        })
    }

    /// Re-reads the files when either changed since the last load. A broken
    /// replacement is reported and the certificate in use kept. Returns
    /// whether a new certificate was installed.
    pub(crate) fn reload_if_changed(&self) -> bool {
        let now = stamp(&self.cert_path, &self.key_path);
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        if *seen == now {
            return false;
        }
        *seen = now;
        match read(&self.cert_path, &self.key_path) {
            Ok(fresh) => {
                *self.current.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(fresh);
                tracing::info!(path = %self.cert_path.display(), "TLS certificate reloaded");
                true
            }
            Err(e) => {
                tracing::warn!(%e, "TLS certificate changed but cannot be loaded; keeping the current one");
                false
            }
        }
    }

    /// Checks for renewals in the background for as long as `cert` is in use.
    pub(crate) fn watch(cert: &Arc<Self>) {
        Self::watch_every(cert, CHECK_INTERVAL);
    }

    fn watch_every(cert: &Arc<Self>, interval: Duration) {
        let weak = Arc::downgrade(cert);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.tick().await;
            loop {
                tick.tick().await;
                let Some(cert) = weak.upgrade() else {
                    return;
                };
                let _ = tokio::task::spawn_blocking(move || cert.reload_if_changed()).await;
            }
        });
    }
}

impl ResolvesServerCert for PemCert {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(Arc::clone(
            &self.current.read().unwrap_or_else(PoisonError::into_inner),
        ))
    }
}

fn stamp(cert_path: &PathBuf, key_path: &PathBuf) -> Stamp {
    let modified = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    (modified(cert_path), modified(key_path))
}

fn transport(context: &str, e: impl std::fmt::Display) -> Error {
    Error::Transport(format!("{context}: {e}"))
}

fn read(cert_path: &PathBuf, key_path: &PathBuf) -> Result<CertifiedKey> {
    let open = |path: &PathBuf| {
        File::open(path)
            .map(BufReader::new)
            .map_err(|e| transport(&format!("failed to open {}", path.display()), e))
    };
    let certs = rustls_pemfile::certs(&mut open(cert_path)?)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| {
            transport(
                &format!("invalid certificate in {}", cert_path.display()),
                e,
            )
        })?;
    if certs.is_empty() {
        return Err(Error::Transport(format!(
            "no certificate in {}",
            cert_path.display()
        )));
    }
    let key = rustls_pemfile::private_key(&mut open(key_path)?)
        .map_err(|e| transport(&format!("invalid private key in {}", key_path.display()), e))?
        .ok_or_else(|| Error::Transport(format!("no private key in {}", key_path.display())))?;
    let signing_key = rustls::crypto::ring::sign::any_supported_type(&key).map_err(|e| {
        transport(
            &format!("unusable private key in {}", key_path.display()),
            e,
        )
    })?;
    let certified = CertifiedKey::new(certs, signing_key);
    // Files replaced one after the other can be read halfway: a new
    // certificate with the old key. Refused, it is retried once the key
    // changes too.
    certified.keys_match().map_err(|e| {
        transport(
            &format!(
                "{} does not match the key in {}",
                cert_path.display(),
                key_path.display()
            ),
            e,
        )
    })?;
    Ok(certified)
}

#[cfg(test)]
mod tests {
    use std::fs::FileTimes;

    use super::*;

    /// A fresh self-signed pair, as PEM.
    fn pair() -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["localhost".to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        (cert.pem(), key.serialize_pem())
    }

    /// Writes `contents` and stamps it `age_secs` after a fixed epoch, so
    /// the test does not depend on the filesystem's clock resolution.
    fn write(path: &PathBuf, contents: &str, age_secs: u64) {
        std::fs::write(path, contents).unwrap();
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + age_secs);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(at))
            .unwrap();
    }

    fn served(cert: &PemCert) -> Vec<u8> {
        cert.current.read().unwrap().cert[0].to_vec()
    }

    #[test]
    fn a_renewed_certificate_replaces_the_served_one() {
        let dir = tempfile::tempdir().unwrap();
        let (cert_path, key_path) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        let (first_cert, first_key) = pair();
        write(&cert_path, &first_cert, 0);
        write(&key_path, &first_key, 0);
        let cert = PemCert::load(cert_path.to_str().unwrap(), key_path.to_str().unwrap()).unwrap();
        let first = served(&cert);
        assert!(!cert.reload_if_changed(), "nothing changed yet");

        let (second_cert, second_key) = pair();
        write(&key_path, &second_key, 60);
        write(&cert_path, &second_cert, 60);
        assert!(cert.reload_if_changed());
        let second = served(&cert);
        assert_ne!(first, second);

        let (third_cert, third_key) = pair();
        write(&cert_path, &third_cert, 90);
        assert!(
            !cert.reload_if_changed(),
            "a certificate without its key yet is not installed"
        );
        assert_eq!(served(&cert), second);
        write(&key_path, &third_key, 90);
        assert!(cert.reload_if_changed(), "installed once its key arrives");
        let third = served(&cert);
        assert_ne!(third, second);

        write(&cert_path, "not a certificate", 120);
        assert!(
            !cert.reload_if_changed(),
            "a broken renewal is not installed"
        );
        assert_eq!(served(&cert), third, "the working certificate stays");
    }

    #[tokio::test]
    async fn the_watcher_installs_a_renewal_while_the_certificate_is_in_use() {
        let dir = tempfile::tempdir().unwrap();
        let (cert_path, key_path) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        let (first_cert, first_key) = pair();
        write(&cert_path, &first_cert, 0);
        write(&key_path, &first_key, 0);
        let cert = Arc::new(
            PemCert::load(cert_path.to_str().unwrap(), key_path.to_str().unwrap()).unwrap(),
        );
        let first = served(&cert);
        PemCert::watch_every(&cert, Duration::from_millis(5));

        let (second_cert, second_key) = pair();
        write(&key_path, &second_key, 60);
        write(&cert_path, &second_cert, 60);
        let renewed = async {
            while served(&cert) == first {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), renewed)
            .await
            .expect("the renewal is picked up");
        // Once nobody holds the certificate, the watcher ends on its next tick.
        drop(cert);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
