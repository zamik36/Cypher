use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use x25519_dalek::StaticSecret;

pub fn load_or_create_onion_identity(path: &Path) -> anyhow::Result<StaticSecret> {
    if let Some(existing) = read_identity(path)? {
        return Ok(StaticSecret::from(existing));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let secret_bytes: [u8; 32] = rand::random();
    let temp_path = temp_path(path);
    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .with_context(|| format!("failed to create {}", temp_path.display()))?;
        file.write_all(&secret_bytes)
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temp_path.display()))?;
    }
    restrict_file_permissions(&temp_path)?;

    rename_or_reuse(&temp_path, path)?;
    let persisted = read_identity(path)?
        .ok_or_else(|| anyhow::anyhow!("missing generated relay identity {}", path.display()))?;
    Ok(StaticSecret::from(persisted))
}

fn read_identity(path: &Path) -> anyhow::Result<Option<[u8; 32]>> {
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let secret: [u8; 32] = data
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid secret length in {}", path.display()))?;
    Ok(Some(secret))
}

fn rename_or_reuse(temp_path: &Path, final_path: &Path) -> anyhow::Result<()> {
    match fs::rename(temp_path, final_path) {
        Ok(()) => Ok(()),
        Err(_) if final_path.exists() => {
            let _ = fs::remove_file(temp_path);
            Ok(())
        }
        Err(e) => Err(e).with_context(|| {
            format!(
                "failed to move generated relay identity {} -> {}",
                temp_path.display(),
                final_path.display()
            )
        }),
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let mut temp = path.as_os_str().to_os_string();
    temp.push(".tmp");
    PathBuf::from(temp)
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use x25519_dalek::PublicKey as X25519PublicKey;

    #[test]
    fn onion_identity_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("onion_identity.bin");

        let first = load_or_create_onion_identity(&path).unwrap();
        let second = load_or_create_onion_identity(&path).unwrap();

        assert_eq!(
            X25519PublicKey::from(&first).as_bytes(),
            X25519PublicKey::from(&second).as_bytes()
        );
    }
}
