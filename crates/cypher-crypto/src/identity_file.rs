//! Passphrase-protected identity blob shared by every client platform.
//!
//! Layout: `version(1) ‖ salt(16) ‖ m_kib(4) ‖ t(4) ‖ p(4) ‖ nonce(12) ‖
//! AES-256-GCM(seed(32) ‖ nickname)` keyed by Argon2id. The header is the
//! AEAD associated data, so the KDF parameters cannot be downgraded.

use argon2::{Algorithm, Argon2, Params, Version};
use rand_core::CryptoRngCore;
use zeroize::Zeroizing;

use crate::aead;
use crate::error::CryptoError;
use crate::identity::IdentitySeed;
use crate::reader::Reader;

const VERSION: u8 = 2;
const SALT_LEN: usize = 16;
const HEADER_LEN: usize = 1 + SALT_LEN + 12 + aead::NONCE_LEN;
pub const MIN_PASSPHRASE_CHARS: usize = 12;
pub const MAX_NICKNAME_BYTES: usize = 64;

const M_KIB: u32 = 64 * 1024;
const T_COST: u32 = 3;
const P_COST: u32 = 1;
const MAX_M_KIB: u32 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdentityFileError {
    #[error("passphrase is too short")]
    WeakPassphrase,
    #[error("nickname is too long")]
    InvalidNickname,
    #[error("wrong passphrase")]
    WrongPassphrase,
    #[error("identity data is corrupt")]
    Corrupt,
}

pub fn seal(
    seed: &IdentitySeed,
    nickname: &str,
    passphrase: &str,
    rng: &mut impl CryptoRngCore,
) -> Result<Vec<u8>, IdentityFileError> {
    if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(IdentityFileError::WeakPassphrase);
    }
    if nickname.len() > MAX_NICKNAME_BYTES {
        return Err(IdentityFileError::InvalidNickname);
    }
    let mut salt = [0u8; SALT_LEN];
    rng.fill_bytes(&mut salt);
    let mut nonce = [0u8; aead::NONCE_LEN];
    rng.fill_bytes(&mut nonce);

    let mut out = Vec::with_capacity(
        nickname
            .len()
            .saturating_add(HEADER_LEN + 32 + aead::TAG_LEN),
    );
    out.push(VERSION);
    out.extend_from_slice(&salt);
    for p in [M_KIB, T_COST, P_COST] {
        out.extend_from_slice(&p.to_le_bytes());
    }
    out.extend_from_slice(&nonce);

    let key = derive_key(passphrase, &salt, M_KIB, T_COST, P_COST)?;
    let mut plain = Zeroizing::new(Vec::with_capacity(nickname.len().saturating_add(32)));
    plain.extend_from_slice(seed.as_bytes());
    plain.extend_from_slice(nickname.as_bytes());
    let ciphertext = aead::seal(&key, &nonce, &out, &plain);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn open(data: &[u8], passphrase: &str) -> Result<(IdentitySeed, String), IdentityFileError> {
    let corrupt = |_: CryptoError| IdentityFileError::Corrupt;
    let (header, ciphertext) = data
        .split_at_checked(HEADER_LEN)
        .ok_or(IdentityFileError::Corrupt)?;
    let mut r = Reader::new(header);
    if r.u8().map_err(corrupt)? != VERSION {
        return Err(IdentityFileError::Corrupt);
    }
    let salt: [u8; SALT_LEN] = r.array().map_err(corrupt)?;
    let (m, t, p) = (
        r.u32().map_err(corrupt)?,
        r.u32().map_err(corrupt)?,
        r.u32().map_err(corrupt)?,
    );
    let nonce: [u8; aead::NONCE_LEN] = r.array().map_err(corrupt)?;
    r.finish().map_err(corrupt)?;
    if m > MAX_M_KIB || ciphertext.len() < 32 + aead::TAG_LEN {
        return Err(IdentityFileError::Corrupt);
    }
    let key = derive_key(passphrase, &salt, m, t, p)?;
    let plain = Zeroizing::new(
        aead::open(&key, &nonce, header, ciphertext)
            .map_err(|_| IdentityFileError::WrongPassphrase)?,
    );
    let (seed, nickname) = plain
        .split_first_chunk::<32>()
        .ok_or(IdentityFileError::Corrupt)?;
    let nickname = String::from_utf8(nickname.to_vec()).map_err(|_| IdentityFileError::Corrupt)?;
    Ok((IdentitySeed(*seed), nickname))
}

fn derive_key(
    passphrase: &str,
    salt: &[u8],
    m: u32,
    t: u32,
    p: u32,
) -> Result<Zeroizing<[u8; 32]>, IdentityFileError> {
    let params = Params::new(m, t, p, Some(32)).map_err(|_| IdentityFileError::Corrupt)?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|_| IdentityFileError::Corrupt)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    const PASS: &str = "correct horse battery";

    #[test]
    #[cfg_attr(miri, ignore = "Argon2id over 64 MiB takes tens of minutes under Miri")]
    fn roundtrip_and_wrong_passphrase() {
        let seed = IdentitySeed::generate();
        let blob = seal(&seed, "alice", PASS, &mut OsRng).unwrap();
        let (back, nick) = open(&blob, PASS).unwrap();
        assert_eq!(back.as_bytes(), seed.as_bytes());
        assert_eq!(nick, "alice");
        assert_eq!(
            open(&blob, "wrong passphrase!").err(),
            Some(IdentityFileError::WrongPassphrase)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "Argon2id over 64 MiB takes tens of minutes under Miri")]
    fn rejects_weak_input_and_tampering() {
        let seed = IdentitySeed::generate();
        assert_eq!(
            seal(&seed, "a", "short", &mut OsRng).err(),
            Some(IdentityFileError::WeakPassphrase)
        );
        assert_eq!(
            seal(&seed, &"x".repeat(65), PASS, &mut OsRng).err(),
            Some(IdentityFileError::InvalidNickname)
        );
        let mut blob = seal(&seed, "a", PASS, &mut OsRng).unwrap();
        blob[1] ^= 1;
        assert!(open(&blob, PASS).is_err());
        assert_eq!(
            open(&blob[..10], PASS).err(),
            Some(IdentityFileError::Corrupt)
        );
    }
}
