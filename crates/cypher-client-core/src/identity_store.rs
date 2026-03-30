//! Persistent identity storage with passphrase-based encryption.
//!
//! The identity seed is encrypted at rest using Argon2id + AES-256-GCM.
//! File format: `[salt: 16][nonce: 12][ciphertext: AES-256-GCM(seed(32) || nickname_len(4) || nickname)]`

use std::fs;
use std::path::{Path, PathBuf};

use argon2::Argon2;
use cypher_common::{Error, Result};
use cypher_crypto::IdentitySeed;
use rand::rngs::OsRng;
use rand::RngCore;
use zeroize::Zeroize;

use crate::persistence::encryption;

const SALT_LEN: usize = 16;
const SEED_LEN: usize = 32;
const IDENTITY_FILE: &str = "identity.enc";

/// Manages reading/writing the encrypted identity seed to disk.
pub struct IdentityStore {
    path: PathBuf,
}

impl IdentityStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join(IDENTITY_FILE),
        }
    }

    pub fn has_identity(&self) -> bool {
        self.path.exists()
    }

    /// Generate a new seed, encrypt it with the passphrase, and write to disk.
    pub fn create(&self, nickname: &str, passphrase: &str) -> Result<IdentitySeed> {
        let seed = IdentitySeed::generate();
        self.save_inner(&seed, nickname, passphrase)?;
        Ok(seed)
    }

    /// Decrypt the stored identity and return the seed + nickname.
    pub fn unlock(&self, passphrase: &str) -> Result<(IdentitySeed, String)> {
        let data = fs::read(&self.path)
            .map_err(|e| Error::Crypto(format!("read identity file: {e}")))?;

        let min_len = SALT_LEN + encryption::NONCE_LEN + SEED_LEN + 4;
        if data.len() < min_len {
            return Err(Error::Crypto("identity file too short".into()));
        }

        let (salt, rest) = data.split_at(SALT_LEN);
        let (nonce_bytes, ciphertext) = rest.split_at(encryption::NONCE_LEN);

        let key = derive_key(passphrase, salt)?;
        let plaintext = encryption::decrypt(&key, ciphertext, nonce_bytes)
            .map_err(|_| Error::Crypto("wrong passphrase or corrupted identity file".into()))?;

        parse_plaintext(&plaintext)
    }

    /// Export the seed as a BIP39 mnemonic (requires passphrase to decrypt).
    pub fn export_mnemonic(&self, passphrase: &str) -> Result<String> {
        let (seed, _) = self.unlock(passphrase)?;
        Ok(seed.to_mnemonic())
    }

    /// Import a seed from a BIP39 mnemonic, encrypt, and save.
    pub fn import_mnemonic(
        &self,
        mnemonic: &str,
        nickname: &str,
        passphrase: &str,
    ) -> Result<IdentitySeed> {
        let seed = IdentitySeed::from_mnemonic(mnemonic)?;
        self.save_inner(&seed, nickname, passphrase)?;
        Ok(seed)
    }

    fn save_inner(&self, seed: &IdentitySeed, nickname: &str, passphrase: &str) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| Error::Crypto(format!("create data dir: {e}")))?;
        }

        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);

        let plaintext = build_plaintext(seed, nickname);
        let mut key = derive_key(passphrase, &salt)?;
        let (ciphertext, nonce) = encryption::encrypt(&key, &plaintext)?;
        key.zeroize();

        let mut output = Vec::with_capacity(SALT_LEN + encryption::NONCE_LEN + ciphertext.len());
        output.extend_from_slice(&salt);
        output.extend_from_slice(&nonce);
        output.extend_from_slice(&ciphertext);

        fs::write(&self.path, &output)
            .map_err(|e| Error::Crypto(format!("write identity file: {e}")))?;
        Ok(())
    }
}

/// Build the plaintext blob: `seed(32) || nickname_len(4 LE) || nickname`.
fn build_plaintext(seed: &IdentitySeed, nickname: &str) -> Vec<u8> {
    let nick = nickname.as_bytes();
    let mut buf = Vec::with_capacity(SEED_LEN + 4 + nick.len());
    buf.extend_from_slice(seed.as_bytes());
    buf.extend_from_slice(&(nick.len() as u32).to_le_bytes());
    buf.extend_from_slice(nick);
    buf
}

/// Parse the decrypted plaintext back into (seed, nickname).
fn parse_plaintext(data: &[u8]) -> Result<(IdentitySeed, String)> {
    if data.len() < SEED_LEN + 4 {
        return Err(Error::Crypto("decrypted data too short".into()));
    }

    let mut seed_bytes = [0u8; SEED_LEN];
    seed_bytes.copy_from_slice(&data[..SEED_LEN]);

    let nick_len = u32::from_le_bytes(
        data[SEED_LEN..SEED_LEN + 4]
            .try_into()
            .map_err(|_| Error::Crypto("bad nickname length".into()))?,
    ) as usize;

    if data.len() < SEED_LEN + 4 + nick_len {
        return Err(Error::Crypto("nickname data truncated".into()));
    }

    let nickname = String::from_utf8(data[SEED_LEN + 4..SEED_LEN + 4 + nick_len].to_vec())
        .map_err(|e| Error::Crypto(format!("invalid nickname utf-8: {e}")))?;

    Ok((IdentitySeed(seed_bytes), nickname))
}

/// Derive a 32-byte encryption key from a passphrase + salt using Argon2id.
fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| Error::Crypto(format!("Argon2id: {e}")))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, IdentityStore) {
        let dir = TempDir::new().unwrap();
        let store = IdentityStore::new(dir.path());
        (dir, store)
    }

    #[test]
    fn create_and_unlock() {
        let (_dir, store) = setup();
        assert!(!store.has_identity());

        let seed = store.create("alice", "mypin123").unwrap();
        assert!(store.has_identity());

        let (restored, nickname) = store.unlock("mypin123").unwrap();
        assert_eq!(seed.as_bytes(), restored.as_bytes());
        assert_eq!(nickname, "alice");
        assert_eq!(
            seed.derive_identity().peer_id().as_bytes(),
            restored.derive_identity().peer_id().as_bytes(),
        );
    }

    #[test]
    fn wrong_passphrase_fails() {
        let (_dir, store) = setup();
        store.create("bob", "correct_pin").unwrap();
        assert!(store.unlock("wrong_pin").is_err());
    }

    #[test]
    fn mnemonic_export_import() {
        let (_dir, store) = setup();
        let seed = store.create("charlie", "pin456").unwrap();
        let mnemonic = store.export_mnemonic("pin456").unwrap();

        let dir2 = TempDir::new().unwrap();
        let store2 = IdentityStore::new(dir2.path());
        let imported = store2.import_mnemonic(&mnemonic, "charlie", "newpin").unwrap();

        assert_eq!(seed.as_bytes(), imported.as_bytes());
    }

    #[test]
    fn unicode_nickname() {
        let (_dir, store) = setup();
        store.create("Алиса 🔐", "pin").unwrap();
        let (_, nickname) = store.unlock("pin").unwrap();
        assert_eq!(nickname, "Алиса 🔐");
    }
}
