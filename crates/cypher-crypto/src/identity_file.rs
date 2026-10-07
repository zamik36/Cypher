//! Passphrase-protected identity blob shared by every client platform.
//!
//! Layout: `version(1) ‖ salt(16) ‖ m_kib(4) ‖ t(4) ‖ p(4) ‖ nonce(12) ‖
//! AES-256-GCM(seed(32) ‖ device(4) ‖ nickname)` keyed by Argon2id. The
//! header is the AEAD associated data, so the KDF parameters cannot be
//! downgraded. Version 2 had no device: such a file is the identity's first
//! device.

use std::ops::RangeInclusive;

use argon2::{Algorithm, Argon2, Params, Version};
use cypher_types::DeviceId;
use rand_core::CryptoRngCore;
use zeroize::Zeroizing;

use crate::aead;
use crate::error::CryptoError;
use crate::identity::IdentitySeed;
use crate::reader::Reader;

const VERSION: u8 = 3;
/// Files from before an identity could have several devices.
const VERSION_WITHOUT_DEVICE: u8 = 2;
const SALT_LEN: usize = 16;
const HEADER_LEN: usize = 1 + SALT_LEN + 12 + aead::NONCE_LEN;
pub const MIN_PASSPHRASE_CHARS: usize = 12;
pub const MAX_NICKNAME_BYTES: usize = 64;

const M_KIB: u32 = 64 * 1024;
const T_COST: u32 = 3;
const P_COST: u32 = 1;

/// Argon2 parameters a file may ask for. The floor keeps a passphrase
/// expensive to guess (OWASP minimum); the ceiling keeps a crafted file from
/// hanging the client or exhausting memory (a browser worker included).
const M_KIB_RANGE: RangeInclusive<u32> = 19 * 1024..=256 * 1024;
const T_COST_RANGE: RangeInclusive<u32> = 2..=8;
const P_COST_RANGE: RangeInclusive<u32> = 1..=4;

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

/// What an identity file holds: the identity, which of its devices this
/// install is, and the name the user goes by.
pub struct Unsealed {
    pub seed: IdentitySeed,
    pub device: DeviceId,
    pub nickname: String,
}

pub fn seal(
    seed: &IdentitySeed,
    device: DeviceId,
    nickname: &str,
    passphrase: &str,
    rng: &mut impl CryptoRngCore,
) -> Result<Vec<u8>, IdentityFileError> {
    let mut plain = Zeroizing::new(Vec::with_capacity(nickname.len().saturating_add(36)));
    plain.extend_from_slice(seed.as_bytes());
    plain.extend_from_slice(&device.0.to_le_bytes());
    plain.extend_from_slice(nickname.as_bytes());
    seal_plain(VERSION, &plain, nickname, passphrase, rng)
}

fn seal_plain(
    version: u8,
    plain: &[u8],
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

    let mut out = Vec::with_capacity(plain.len().saturating_add(HEADER_LEN + aead::TAG_LEN));
    out.push(version);
    out.extend_from_slice(&salt);
    for p in [M_KIB, T_COST, P_COST] {
        out.extend_from_slice(&p.to_le_bytes());
    }
    out.extend_from_slice(&nonce);

    let key = derive_key(passphrase, &salt, M_KIB, T_COST, P_COST)?;
    let ciphertext = aead::seal(&key, &nonce, &out, plain);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn open(data: &[u8], passphrase: &str) -> Result<Unsealed, IdentityFileError> {
    let corrupt = |_: CryptoError| IdentityFileError::Corrupt;
    let (header, ciphertext) = data
        .split_at_checked(HEADER_LEN)
        .ok_or(IdentityFileError::Corrupt)?;
    let mut r = Reader::new(header);
    let version = r.u8().map_err(corrupt)?;
    if version != VERSION && version != VERSION_WITHOUT_DEVICE {
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
    let params_allowed =
        M_KIB_RANGE.contains(&m) && T_COST_RANGE.contains(&t) && P_COST_RANGE.contains(&p);
    if !params_allowed || ciphertext.len() < 32 + aead::TAG_LEN {
        return Err(IdentityFileError::Corrupt);
    }
    let key = derive_key(passphrase, &salt, m, t, p)?;
    let plain = Zeroizing::new(
        aead::open(&key, &nonce, header, ciphertext)
            .map_err(|_| IdentityFileError::WrongPassphrase)?,
    );
    let (seed, rest) = plain
        .split_first_chunk::<32>()
        .ok_or(IdentityFileError::Corrupt)?;
    let (device, nickname) = if version == VERSION_WITHOUT_DEVICE {
        (DeviceId::FIRST, rest)
    } else {
        let (device, nickname) = rest
            .split_first_chunk::<4>()
            .ok_or(IdentityFileError::Corrupt)?;
        (DeviceId(u32::from_le_bytes(*device)), nickname)
    };
    if !device.is_valid() {
        return Err(IdentityFileError::Corrupt);
    }
    let nickname = String::from_utf8(nickname.to_vec()).map_err(|_| IdentityFileError::Corrupt)?;
    Ok(Unsealed {
        seed: IdentitySeed(*seed),
        device,
        nickname,
    })
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
        let blob = seal(&seed, DeviceId(7), "alice", PASS, &mut OsRng).unwrap();
        let back = open(&blob, PASS).unwrap();
        assert_eq!(back.seed.as_bytes(), seed.as_bytes());
        assert_eq!(
            (back.device, back.nickname.as_str()),
            (DeviceId(7), "alice")
        );
        assert_eq!(
            open(&blob, "wrong passphrase!").err(),
            Some(IdentityFileError::WrongPassphrase)
        );
    }

    /// A file from before devices is its identity's first device.
    #[test]
    #[cfg_attr(miri, ignore = "Argon2id over 64 MiB takes tens of minutes under Miri")]
    fn version_2_files_open_as_the_first_device() {
        let seed = IdentitySeed::generate();
        let plain = [seed.as_bytes().as_slice(), b"bob"].concat();
        let blob = seal_plain(VERSION_WITHOUT_DEVICE, &plain, "bob", PASS, &mut OsRng).unwrap();
        let back = open(&blob, PASS).unwrap();
        assert_eq!(back.seed.as_bytes(), seed.as_bytes());
        assert_eq!(
            (back.device, back.nickname.as_str()),
            (DeviceId::FIRST, "bob")
        );

        let zero = [seed.as_bytes().as_slice(), &[0; 4], b"bob"].concat();
        let blob = seal_plain(VERSION, &zero, "bob", PASS, &mut OsRng).unwrap();
        assert_eq!(open(&blob, PASS).err(), Some(IdentityFileError::Corrupt));
    }

    #[test]
    #[cfg_attr(miri, ignore = "Argon2id over 64 MiB takes tens of minutes under Miri")]
    fn rejects_weak_input_and_tampering() {
        let seed = IdentitySeed::generate();
        assert_eq!(
            seal(&seed, DeviceId::FIRST, "a", "short", &mut OsRng).err(),
            Some(IdentityFileError::WeakPassphrase)
        );
        assert_eq!(
            seal(&seed, DeviceId::FIRST, &"x".repeat(65), PASS, &mut OsRng).err(),
            Some(IdentityFileError::InvalidNickname)
        );
        let mut blob = seal(&seed, DeviceId::FIRST, "a", PASS, &mut OsRng).unwrap();
        blob[1] ^= 1;
        assert!(open(&blob, PASS).is_err());
        assert_eq!(
            open(&blob[..10], PASS).err(),
            Some(IdentityFileError::Corrupt)
        );
    }

    /// Parameters outside the allowed range are refused before Argon2 runs:
    /// too weak to protect the passphrase, or so costly the client would hang.
    #[test]
    #[cfg_attr(miri, ignore = "Argon2id over 64 MiB takes tens of minutes under Miri")]
    fn refuses_kdf_parameters_out_of_range() {
        const M_AT: usize = 1 + SALT_LEN;
        let blob = seal(
            &IdentitySeed::generate(),
            DeviceId::FIRST,
            "a",
            PASS,
            &mut OsRng,
        )
        .unwrap();
        for (offset, value) in [
            (M_AT, 8),
            (M_AT, u32::MAX),
            (M_AT + 4, 1),
            (M_AT + 4, u32::MAX),
            (M_AT + 8, 64),
        ] {
            let mut crafted = blob.clone();
            crafted[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert_eq!(
                open(&crafted, PASS).err(),
                Some(IdentityFileError::Corrupt),
                "parameter at {offset} = {value}"
            );
        }
        assert!(
            M_KIB_RANGE.contains(&M_KIB)
                && T_COST_RANGE.contains(&T_COST)
                && P_COST_RANGE.contains(&P_COST)
        );
    }
}
