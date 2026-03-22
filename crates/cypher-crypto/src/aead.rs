use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use cypher_common::{Error, Result};
use hkdf::Hkdf;
use sha2::Sha256;

/// Derive a 12-byte nonce from arbitrary nonce material using HKDF.
fn derive_nonce(nonce_material: &[u8]) -> [u8; 12] {
    let hk = Hkdf::<Sha256>::new(Some(b"cypher-aead-nonce"), nonce_material);
    let mut nonce = [0u8; 12];
    hk.expand(b"aes-gcm-nonce", &mut nonce)
        .expect("12 bytes is a valid HKDF-SHA256 output length");
    nonce
}

/// Encrypt plaintext with AES-256-GCM.
///
/// Returns ciphertext with appended authentication tag.
/// The 12-byte nonce is derived from `nonce_material` via HKDF-SHA256.
pub fn aead_encrypt(
    key: &[u8; 32],
    nonce_material: &[u8],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(key.into());
    let nonce_bytes = derive_nonce(nonce_material);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let payload = aes_gcm::aead::Payload {
        msg: plaintext,
        aad,
    };

    cipher
        .encrypt(nonce, payload)
        .map_err(|e| Error::Crypto(format!("AES-GCM encryption failed: {}", e)))
}

/// Decrypt ciphertext with AES-256-GCM.
///
/// The `ciphertext` must include the appended authentication tag.
/// The 12-byte nonce is derived from `nonce_material` via HKDF-SHA256.
pub fn aead_decrypt(
    key: &[u8; 32],
    nonce_material: &[u8],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(key.into());
    let nonce_bytes = derive_nonce(nonce_material);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let payload = aes_gcm::aead::Payload {
        msg: ciphertext,
        aad,
    };

    cipher
        .decrypt(nonce, payload)
        .map_err(|e| Error::Crypto(format!("AES-GCM decryption failed: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = [42u8; 32];
        let nonce_material = b"test-nonce-material";
        let plaintext = b"hello world";
        let aad = b"additional data";

        let ciphertext = aead_encrypt(&key, nonce_material, plaintext, aad).unwrap();
        let decrypted = aead_decrypt(&key, nonce_material, &ciphertext, aad).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let key = [42u8; 32];
        let wrong_key = [43u8; 32];
        let nonce_material = b"nonce";
        let plaintext = b"secret";
        let aad = b"";

        let ciphertext = aead_encrypt(&key, nonce_material, plaintext, aad).unwrap();
        let result = aead_decrypt(&wrong_key, nonce_material, &ciphertext, aad);

        assert!(result.is_err());
    }

    #[test]
    fn wrong_aad_fails() {
        let key = [42u8; 32];
        let nonce_material = b"nonce";
        let plaintext = b"secret";

        let ciphertext = aead_encrypt(&key, nonce_material, plaintext, b"aad1").unwrap();
        let result = aead_decrypt(&key, nonce_material, &ciphertext, b"aad2");

        assert!(result.is_err());
    }
}
