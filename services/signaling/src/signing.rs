use std::path::Path;

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::key_store::load_or_create_secret;

type HmacSha256 = Hmac<Sha256>;

/// Server-side signer for inbox responses (Ed25519) and claim tokens (HMAC).
pub struct ServerSigner {
    signing_key: SigningKey,
    hmac_secret: [u8; 32],
}

impl ServerSigner {
    /// Create a new signer from raw key material.
    ///
    /// In production, load key material from the persisted local key store.
    pub fn new(signing_seed: [u8; 32], hmac_secret: [u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(&signing_seed);
        Self {
            signing_key,
            hmac_secret,
        }
    }

    pub fn load_or_create(
        signing_seed_path: &Path,
        hmac_secret_path: &Path,
    ) -> anyhow::Result<Self> {
        let signing_seed = load_or_create_secret(signing_seed_path)?;
        let hmac_secret = load_or_create_secret(hmac_secret_path)?;
        Ok(Self::new(signing_seed, hmac_secret))
    }

    pub fn load_or_create_default() -> anyhow::Result<Self> {
        Self::load_or_create(
            Path::new("/data/signaling/inbox_signing.bin"),
            Path::new("/data/signaling/inbox_hmac.bin"),
        )
    }

    #[allow(dead_code)]
    pub fn new_for_tests(signing_seed: [u8; 32], hmac_secret: [u8; 32]) -> Self {
        Self::new(signing_seed, hmac_secret)
    }

    /// Ed25519 public key (to be pinned in client binary).
    #[allow(dead_code)]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Sign an inbox response.
    ///
    /// Signs the concatenation: `messages_blob || count (4B LE) || inbox_id || timestamp (8B LE)`.
    pub fn sign_inbox_response(
        &self,
        messages_blob: &[u8],
        count: u32,
        inbox_id: &[u8],
        timestamp: u64,
    ) -> [u8; 64] {
        let mut data = Vec::with_capacity(messages_blob.len() + 4 + inbox_id.len() + 8);
        data.extend_from_slice(messages_blob);
        data.extend_from_slice(&count.to_le_bytes());
        data.extend_from_slice(inbox_id);
        data.extend_from_slice(&timestamp.to_le_bytes());

        let sig = self.signing_key.sign(&data);
        sig.to_bytes()
    }

    /// Generate a claim token for two-phase inbox fetch.
    ///
    /// Format: `[timestamp 8B LE][HMAC-SHA256(hmac_secret, inbox_id || timestamp) 32B]`
    /// Total: 40 bytes. The timestamp is embedded so the verifier can extract it.
    pub fn generate_claim_token(&self, inbox_id: &[u8], timestamp: u64) -> Vec<u8> {
        let mut mac =
            HmacSha256::new_from_slice(&self.hmac_secret).expect("HMAC key can be any size");
        mac.update(inbox_id);
        mac.update(&timestamp.to_le_bytes());
        let hmac_bytes: [u8; 32] = mac.finalize().into_bytes().into();

        let mut token = Vec::with_capacity(40);
        token.extend_from_slice(&timestamp.to_le_bytes());
        token.extend_from_slice(&hmac_bytes);
        token
    }

    /// Verify a claim token (40 bytes: 8B timestamp + 32B HMAC).
    ///
    /// Rejects tokens older than `max_age_secs` to prevent replay attacks.
    pub fn verify_claim_token(&self, inbox_id: &[u8], token: &[u8], max_age_secs: u64) -> bool {
        if token.len() != 40 {
            return false;
        }
        let timestamp = u64::from_le_bytes(token[..8].try_into().unwrap());

        // Reject expired tokens.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now.saturating_sub(timestamp) > max_age_secs {
            return false;
        }
        // Reject tokens from the future (clock skew tolerance: 60s).
        if timestamp > now + 60 {
            return false;
        }

        let expected = self.generate_claim_token(inbox_id, timestamp);
        constant_time_eq(&expected, token)
    }
}

/// Constant-time comparison to prevent timing attacks on token verification.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;

    #[test]
    fn sign_and_verify() {
        let signer = ServerSigner::new_for_tests([1u8; 32], [2u8; 32]);
        let messages = b"some messages blob";
        let count = 3u32;
        let inbox_id = b"inbox_abc";
        let timestamp = 1700000000u64;

        let sig_bytes = signer.sign_inbox_response(messages, count, inbox_id, timestamp);

        // Verify with public key
        let vk = signer.verifying_key();
        let mut data = Vec::new();
        data.extend_from_slice(messages);
        data.extend_from_slice(&count.to_le_bytes());
        data.extend_from_slice(inbox_id);
        data.extend_from_slice(&timestamp.to_le_bytes());

        let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
        assert!(vk.verify(&data, &sig).is_ok());
    }

    #[test]
    fn claim_token_roundtrip() {
        let signer = ServerSigner::new_for_tests([1u8; 32], [2u8; 32]);
        let inbox_id = b"inbox_123";
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let token = signer.generate_claim_token(inbox_id, now);
        assert_eq!(token.len(), 40);
        assert!(signer.verify_claim_token(inbox_id, &token, 300));

        // Tampered token fails
        let mut bad = token.clone();
        bad[39] ^= 0xFF;
        assert!(!signer.verify_claim_token(inbox_id, &bad, 300));

        // Wrong inbox_id fails
        assert!(!signer.verify_claim_token(b"other", &token, 300));

        // Expired token (old timestamp) fails
        let old_token = signer.generate_claim_token(inbox_id, now - 600);
        assert!(!signer.verify_claim_token(inbox_id, &old_token, 300));
    }

    #[test]
    fn persisted_keys_are_stable() {
        let dir = tempfile::tempdir().unwrap();
        let first = ServerSigner::load_or_create(
            &dir.path().join("signing.bin"),
            &dir.path().join("hmac.bin"),
        )
        .unwrap();
        let second = ServerSigner::load_or_create(
            &dir.path().join("signing.bin"),
            &dir.path().join("hmac.bin"),
        )
        .unwrap();

        assert_eq!(first.verifying_key(), second.verifying_key());
    }
}
