//! Passphrase-protected identity file.
//!
//! Layout: `version(1) ‖ salt(16) ‖ m_kib(4) ‖ t(4) ‖ p(4) ‖ nonce(12) ‖
//! AES-256-GCM(seed(32) ‖ nickname)` with the key from Argon2id. The header is
//! authenticated as associated data, so parameters cannot be downgraded.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use cypher_crypto::{IdentitySeed, aead};
use rand::RngCore;
use rand::rngs::OsRng;
use zeroize::Zeroizing;

use crate::ClientError;

const FILE_NAME: &str = "identity.v2";
const VERSION: u8 = 2;
const SALT_LEN: usize = 16;
const HEADER_LEN: usize = 1 + SALT_LEN + 12 + aead::NONCE_LEN;
pub const MIN_PASSPHRASE_CHARS: usize = 12;
const MAX_NICKNAME_BYTES: usize = 64;

const M_KIB: u32 = 64 * 1024;
const T_COST: u32 = 3;
const P_COST: u32 = 1;

pub struct Unlocked {
    pub seed: IdentitySeed,
    pub nickname: String,
}

pub struct IdentityStore {
    path: PathBuf,
}

impl IdentityStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(FILE_NAME),
        }
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Creates a new identity. Refuses to overwrite an existing one: losing
    /// the seed would make every stored conversation unreadable.
    pub fn create(&self, nickname: &str, passphrase: &str) -> Result<Unlocked, ClientError> {
        self.save_new(IdentitySeed::generate(), nickname, passphrase)
    }

    pub fn import(
        &self,
        mnemonic: &str,
        nickname: &str,
        passphrase: &str,
    ) -> Result<Unlocked, ClientError> {
        let seed = IdentitySeed::from_mnemonic(mnemonic.trim())
            .map_err(|_| ClientError::InvalidMnemonic)?;
        self.save_new(seed, nickname, passphrase)
    }

    pub fn unlock(&self, passphrase: &str) -> Result<Unlocked, ClientError> {
        let data = fs::read(&self.path)?;
        if data.len() < HEADER_LEN + aead::TAG_LEN + 32 || data[0] != VERSION {
            return Err(ClientError::CorruptIdentity);
        }
        let (header, ciphertext) = data.split_at(HEADER_LEN);
        let salt = &header[1..1 + SALT_LEN];
        let param = |i: usize| {
            u32::from_le_bytes(
                header[1 + SALT_LEN + i * 4..][..4]
                    .try_into()
                    .unwrap_or_default(),
            )
        };
        let key = derive_key(passphrase, salt, param(0), param(1), param(2))?;
        let nonce: &[u8; aead::NONCE_LEN] = header[HEADER_LEN - aead::NONCE_LEN..]
            .try_into()
            .map_err(|_| ClientError::CorruptIdentity)?;
        let plain = Zeroizing::new(
            aead::open(&key, nonce, header, ciphertext)
                .map_err(|_| ClientError::WrongPassphrase)?,
        );
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&plain[..32]);
        let nickname =
            String::from_utf8(plain[32..].to_vec()).map_err(|_| ClientError::CorruptIdentity)?;
        Ok(Unlocked {
            seed: IdentitySeed(seed),
            nickname,
        })
    }

    /// Removes the identity file; the caller must confirm with the user.
    pub fn delete(&self) -> Result<(), ClientError> {
        match fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    fn save_new(
        &self,
        seed: IdentitySeed,
        nickname: &str,
        passphrase: &str,
    ) -> Result<Unlocked, ClientError> {
        if self.exists() {
            return Err(ClientError::IdentityExists);
        }
        if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
            return Err(ClientError::WeakPassphrase);
        }
        if nickname.len() > MAX_NICKNAME_BYTES {
            return Err(ClientError::InvalidInput);
        }
        let mut header = Vec::with_capacity(HEADER_LEN);
        header.push(VERSION);
        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        header.extend_from_slice(&salt);
        for p in [M_KIB, T_COST, P_COST] {
            header.extend_from_slice(&p.to_le_bytes());
        }
        let mut nonce = [0u8; aead::NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        header.extend_from_slice(&nonce);

        let key = derive_key(passphrase, &salt, M_KIB, T_COST, P_COST)?;
        let mut plain = Zeroizing::new(Vec::with_capacity(32 + nickname.len()));
        plain.extend_from_slice(seed.as_bytes());
        plain.extend_from_slice(nickname.as_bytes());
        let ciphertext = aead::seal(&key, &nonce, &header, &plain);

        let mut file = header;
        file.extend_from_slice(&ciphertext);
        write_atomic(&self.path, &file)?;
        Ok(Unlocked {
            seed,
            nickname: nickname.to_owned(),
        })
    }
}

fn derive_key(
    passphrase: &str,
    salt: &[u8],
    m: u32,
    t: u32,
    p: u32,
) -> Result<Zeroizing<[u8; 32]>, ClientError> {
    let params = Params::new(m, t, p, Some(32)).map_err(|_| ClientError::CorruptIdentity)?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|_| ClientError::CorruptIdentity)?;
    Ok(key)
}

/// Write to a temporary file, fsync, then rename over the target.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<(), ClientError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASS: &str = "correct horse battery";

    #[test]
    fn create_unlock_and_refuse_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = IdentityStore::new(dir.path());
        let created = store.create("alice", PASS).unwrap();
        let unlocked = store.unlock(PASS).unwrap();
        assert_eq!(unlocked.seed.as_bytes(), created.seed.as_bytes());
        assert_eq!(unlocked.nickname, "alice");
        assert!(matches!(
            store.unlock("wrong passphrase!"),
            Err(ClientError::WrongPassphrase)
        ));
        assert!(matches!(
            store.create("bob", PASS),
            Err(ClientError::IdentityExists)
        ));
    }

    #[test]
    fn weak_passphrase_and_bad_mnemonic_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = IdentityStore::new(dir.path());
        assert!(matches!(
            store.create("a", "short"),
            Err(ClientError::WeakPassphrase)
        ));
        assert!(matches!(
            store.import("not words", "a", PASS),
            Err(ClientError::InvalidMnemonic)
        ));
        assert!(!store.exists());
    }

    #[test]
    fn mnemonic_import_restores_the_same_identity() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let original = IdentityStore::new(a.path()).create("x", PASS).unwrap();
        let restored = IdentityStore::new(b.path())
            .import(&original.seed.to_mnemonic(), "x", PASS)
            .unwrap();
        assert_eq!(original.seed.as_bytes(), restored.seed.as_bytes());
    }

    #[test]
    fn tampered_header_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let store = IdentityStore::new(dir.path());
        store.create("a", PASS).unwrap();
        let path = dir.path().join(FILE_NAME);
        let mut data = fs::read(&path).unwrap();
        data[1] ^= 1;
        fs::write(&path, data).unwrap();
        assert!(store.unlock(PASS).is_err());
    }
}
