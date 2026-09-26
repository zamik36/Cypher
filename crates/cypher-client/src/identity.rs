//! Identity file on disk; the format itself lives in `cypher_crypto::identity_file`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use cypher_crypto::IdentitySeed;
use cypher_crypto::identity_file::{self, IdentityFileError};
use rand::rngs::OsRng;

use crate::ClientError;

const FILE_NAME: &str = "identity.v2";

pub struct Unlocked {
    pub seed: IdentitySeed,
    pub nickname: String,
}

pub struct IdentityStore {
    path: PathBuf,
}

impl From<IdentityFileError> for ClientError {
    fn from(e: IdentityFileError) -> Self {
        match e {
            IdentityFileError::WeakPassphrase => Self::WeakPassphrase,
            IdentityFileError::InvalidNickname => Self::InvalidInput,
            IdentityFileError::WrongPassphrase => Self::WrongPassphrase,
            IdentityFileError::Corrupt => Self::CorruptIdentity,
        }
    }
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
        let (seed, nickname) = identity_file::open(&fs::read(&self.path)?, passphrase)?;
        Ok(Unlocked { seed, nickname })
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
        let blob = identity_file::seal(&seed, nickname, passphrase, &mut OsRng)?;
        write_atomic(&self.path, &blob)?;
        Ok(Unlocked {
            seed,
            nickname: nickname.to_owned(),
        })
    }
}

/// Write to a temporary file, fsync, then rename over the target.
fn write_atomic(path: &Path, data: &[u8]) -> Result<(), ClientError> {
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
}
