//! Identity file on disk; the format itself lives in `cypher_crypto::identity_file`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use cypher_core::IdentityKind;
use cypher_crypto::IdentitySeed;
use cypher_crypto::identity_file::{self, IdentityFileError};
use cypher_types::DeviceId;
use rand::rngs::OsRng;

use crate::ClientError;

const FILE_NAME: &str = "identity.v2";

pub use identity_file::Unsealed as Unlocked;

pub struct IdentityStore {
    path: PathBuf,
}

impl From<IdentityFileError> for ClientError {
    fn from(e: IdentityFileError) -> Self {
        Self::Identity(e.into())
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
        self.save_new(
            IdentitySeed::generate(),
            DeviceId::FIRST,
            nickname,
            passphrase,
        )
    }

    pub fn import(
        &self,
        mnemonic: &str,
        nickname: &str,
        passphrase: &str,
    ) -> Result<Unlocked, ClientError> {
        let seed = IdentitySeed::from_mnemonic(mnemonic.trim())
            .map_err(|_| ClientError::Identity(IdentityKind::InvalidMnemonic))?;
        // A new device of the profile: the ones it had may still be in use,
        // and two installs as one device would push each other out.
        self.save_new(seed, DeviceId::random(&mut OsRng), nickname, passphrase)
    }

    /// Keeps an identity another device handed over, as the device it was
    /// given, under a passphrase of this device's own.
    pub fn adopt(
        &self,
        linked: cypher_core::link::Linked,
        passphrase: &str,
    ) -> Result<Unlocked, ClientError> {
        if self.exists() {
            return Err(ClientError::Identity(IdentityKind::Exists));
        }
        let blob = identity_file::seal(
            &linked.seed,
            linked.device,
            &linked.nickname,
            passphrase,
            &mut OsRng,
        )?;
        write_atomic(&self.path, &blob)?;
        Ok(Unlocked {
            seed: linked.seed,
            device: linked.device,
            nickname: linked.nickname,
        })
    }

    /// Deletes everything the app keeps next to the identity: conversations,
    /// contacts, media, then the identity itself, so a failure part way
    /// leaves the profile in place to try again. Files the user saved
    /// elsewhere stay. The client must be stopped first.
    pub fn erase_device(&self) -> Result<(), ClientError> {
        let Some(dir) = self.path.parent() else {
            return Ok(());
        };
        let entries = match fs::read_dir(dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            entries => entries?,
        };
        for entry in entries {
            let path = entry?.path();
            if path == self.path {
                continue;
            }
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
        }
        match fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    pub fn unlock(&self, passphrase: &str) -> Result<Unlocked, ClientError> {
        let sealed = fs::read(&self.path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ClientError::Identity(IdentityKind::Missing),
            _ => e.into(),
        })?;
        Ok(identity_file::open(&sealed, passphrase)?)
    }

    fn save_new(
        &self,
        seed: IdentitySeed,
        device: DeviceId,
        nickname: &str,
        passphrase: &str,
    ) -> Result<Unlocked, ClientError> {
        if self.exists() {
            return Err(ClientError::Identity(IdentityKind::Exists));
        }
        let blob = identity_file::seal(&seed, device, nickname, passphrase, &mut OsRng)?;
        write_atomic(&self.path, &blob)?;
        Ok(Unlocked {
            seed,
            device,
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
            Err(ClientError::Identity(IdentityKind::WrongPassphrase))
        ));
        assert!(matches!(
            store.create("bob", PASS),
            Err(ClientError::Identity(IdentityKind::Exists))
        ));
    }

    /// What the UI is told when there is no identity, or the nickname is
    /// too long: the identity kinds the web client reports for the same.
    #[test]
    fn a_missing_file_and_a_long_nickname_are_identity_failures() {
        use cypher_core::{CmdError, ErrorKind, IdentityKind};

        let dir = tempfile::tempdir().unwrap();
        let store = IdentityStore::new(dir.path());
        let kind = |e: ClientError| CmdError::from(e).kind;
        let missing = store.unlock(PASS).map(|_| ()).unwrap_err();
        assert_eq!(kind(missing), ErrorKind::Identity(IdentityKind::Missing));
        let long = "n".repeat(identity_file::MAX_NICKNAME_BYTES + 1);
        let refused = store.create(&long, PASS).map(|_| ()).unwrap_err();
        assert_eq!(
            kind(refused),
            ErrorKind::Identity(IdentityKind::NicknameTooLong)
        );
        assert!(!store.exists());
    }

    #[test]
    fn erasing_the_device_makes_room_for_a_new_identity() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let store = IdentityStore::new(&data);
        store.create("alice", PASS).unwrap();
        fs::write(data.join("state.db"), b"chats").unwrap();
        fs::create_dir_all(data.join("media")).unwrap();
        fs::write(data.join("media").join("note.bin"), b"voice").unwrap();

        store.erase_device().unwrap();
        assert!(!store.exists());
        assert_eq!(fs::read_dir(&data).unwrap().count(), 0);
        store.create("bob", PASS).unwrap();
        assert_eq!(store.unlock(PASS).unwrap().nickname, "bob");

        let missing = IdentityStore::new(&dir.path().join("never-created"));
        missing.erase_device().unwrap();
    }

    #[test]
    fn weak_passphrase_and_bad_mnemonic_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = IdentityStore::new(dir.path());
        assert!(matches!(
            store.create("a", "short"),
            Err(ClientError::Identity(IdentityKind::WeakPassphrase))
        ));
        assert!(matches!(
            store.import("not words", "a", PASS),
            Err(ClientError::Identity(IdentityKind::InvalidMnemonic))
        ));
        assert!(!store.exists());
    }

    #[test]
    fn mnemonic_import_is_the_same_identity_on_a_new_device() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let original = IdentityStore::new(a.path()).create("x", PASS).unwrap();
        assert_eq!(original.device, DeviceId::FIRST);
        let store = IdentityStore::new(b.path());
        let restored = store
            .import(&original.seed.to_mnemonic(), "x", PASS)
            .unwrap();
        assert_eq!(original.seed.as_bytes(), restored.seed.as_bytes());
        assert!(restored.device.is_valid());
        assert_ne!(restored.device, original.device);
        assert_eq!(store.unlock(PASS).unwrap().device, restored.device);
    }
}
