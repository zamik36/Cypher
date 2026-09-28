use rand::RngCore;
use rand::rngs::OsRng;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;

pub fn load_or_create_secret(path: &Path) -> anyhow::Result<[u8; 32]> {
    if let Some(existing) = read_secret(path)? {
        return Ok(existing);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    let temp_path = temp_path(path, OsRng.next_u64());
    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .with_context(|| format!("failed to create {}", temp_path.display()))?;
        file.write_all(&secret)
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temp_path.display()))?;
    }
    restrict_file_permissions(&temp_path)?;

    rename_or_reuse(&temp_path, path)?;
    read_secret(path)?.ok_or_else(|| anyhow::anyhow!("missing generated secret {}", path.display()))
}

fn read_secret(path: &Path) -> anyhow::Result<Option<[u8; 32]>> {
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
                "failed to move generated secret {} -> {}",
                temp_path.display(),
                final_path.display()
            )
        }),
    }
}

/// A fresh name per attempt: a temp file left by a crash never blocks the
/// next start, and concurrent creators never write the same file.
fn temp_path(path: &Path, nonce: u64) -> PathBuf {
    let mut temp = path.as_os_str().to_os_string();
    temp.push(format!(".{nonce:016x}.tmp"));
    PathBuf::from(temp)
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "same signature as the Unix variant, which can fail"
)]
fn restrict_file_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_missing_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("key.bin");
        load_or_create_secret(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap().len(), 32);
    }

    #[test]
    fn leftover_temp_file_from_a_crash_does_not_block_startup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.bin");
        fs::write(dir.path().join("key.bin.tmp"), b"partial").unwrap();
        fs::write(temp_path(&path, 7), b"partial").unwrap();
        load_or_create_secret(&path).unwrap();
    }

    #[test]
    fn corrupt_secret_is_an_error_not_a_new_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.bin");
        fs::write(&path, [1u8; 31]).unwrap();
        let err = load_or_create_secret(&path).unwrap_err().to_string();
        assert!(err.contains("invalid secret length"), "{err}");
        assert_eq!(fs::read(&path).unwrap(), [1u8; 31], "left untouched");
    }

    #[test]
    fn losing_the_rename_race_keeps_the_winner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.bin");
        fs::write(&path, [9u8; 32]).unwrap();
        rename_or_reuse(&dir.path().join("gone.tmp"), &path).unwrap();
        assert_eq!(load_or_create_secret(&path).unwrap(), [9u8; 32]);
        rename_or_reuse(&dir.path().join("gone.tmp"), &dir.path().join("none")).unwrap_err();
    }

    #[cfg(unix)]
    #[test]
    fn secret_is_readable_by_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.bin");
        load_or_create_secret(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn secret_persists_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signing.bin");

        let first = load_or_create_secret(&path).unwrap();
        let second = load_or_create_secret(&path).unwrap();

        assert_eq!(first, second);
    }
}
