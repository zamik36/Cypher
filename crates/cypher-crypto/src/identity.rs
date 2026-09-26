use cypher_types::PeerId;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::RngCore;
use rand::rngs::OsRng;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::error::CryptoError;
use crate::kdf::hkdf;

/// 256-bit root secret; every identity key is derived from it. Exported and
/// imported as a 24-word BIP39 phrase.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct IdentitySeed(pub [u8; 32]);

impl IdentitySeed {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn to_mnemonic(&self) -> String {
        bip39::Mnemonic::from_entropy(&self.0)
            .expect("32 bytes is valid BIP39 entropy")
            .to_string()
    }

    pub fn from_mnemonic(phrase: &str) -> Result<Self, CryptoError> {
        let mnemonic = bip39::Mnemonic::parse(phrase).map_err(|_| CryptoError::Malformed)?;
        let entropy = Zeroizing::new(mnemonic.to_entropy());
        let bytes: [u8; 32] = entropy
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::Malformed)?;
        Ok(Self(bytes))
    }

    pub fn derive_identity(&self) -> IdentityKeyPair {
        IdentityKeyPair::from_seed(self)
    }

    /// Key for encrypting local state at rest.
    pub fn derive_storage_key(&self) -> [u8; 32] {
        *hkdf::<32>(None, &self.0, b"cypher-storage-key")
    }

    /// Bearer secret for the owner's blind inbox. Peers only ever see
    /// `cypher_wire::inbox_id(secret)`, which grants write but not read access.
    pub fn derive_inbox_secret(&self) -> Zeroizing<[u8; 32]> {
        hkdf::<32>(None, &self.0, b"cypher/v2/inbox-secret")
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Ed25519 signing identity (whose public key is the peer id) plus a separate
/// X25519 key for key agreement.
pub struct IdentityKeyPair {
    pub signing_key: SigningKey,
    pub dh_secret: X25519StaticSecret,
}

impl IdentityKeyPair {
    pub fn generate() -> Self {
        Self {
            signing_key: SigningKey::generate(&mut OsRng),
            dh_secret: X25519StaticSecret::random_from_rng(OsRng),
        }
    }

    pub fn from_seed(seed: &IdentitySeed) -> Self {
        let ed = hkdf::<32>(None, &seed.0, b"cypher-ed25519");
        let dh = hkdf::<32>(None, &seed.0, b"cypher-x25519");
        Self {
            signing_key: SigningKey::from_bytes(&ed),
            dh_secret: X25519StaticSecret::from(*dh),
        }
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    pub fn dh_public_key(&self) -> X25519PublicKey {
        X25519PublicKey::from(&self.dh_secret)
    }

    pub fn peer_id(&self) -> PeerId {
        PeerId(self.verifying_key().to_bytes())
    }

    pub fn sign(&self, data: &[u8]) -> Signature {
        self.signing_key.sign(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;

    #[test]
    fn signatures_verify_against_the_peer_id() {
        let kp = IdentityKeyPair::generate();
        let sig = kp.sign(b"hello");
        let vk = VerifyingKey::from_bytes(kp.peer_id().as_bytes()).unwrap();
        assert!(vk.verify(b"hello", &sig).is_ok());
        assert!(vk.verify(b"world", &sig).is_err());
    }

    #[test]
    fn seed_derivation_is_deterministic_and_separated() {
        let seed = IdentitySeed::generate();
        let (a, b) = (seed.derive_identity(), seed.derive_identity());
        assert_eq!(a.peer_id(), b.peer_id());
        assert_eq!(a.dh_public_key().as_bytes(), b.dh_public_key().as_bytes());
        let sek = seed.derive_storage_key();
        assert_ne!(&sek, a.peer_id().as_bytes());
        assert_ne!(sek, *seed.derive_inbox_secret());
        assert_ne!(
            IdentitySeed::generate().derive_identity().peer_id(),
            a.peer_id()
        );
    }

    #[test]
    fn mnemonic_roundtrip() {
        let seed = IdentitySeed::generate();
        let phrase = seed.to_mnemonic();
        assert_eq!(phrase.split_whitespace().count(), 24);
        let restored = IdentitySeed::from_mnemonic(&phrase).unwrap();
        assert_eq!(seed.0, restored.0);
        assert!(IdentitySeed::from_mnemonic("not a valid mnemonic").is_err());
    }

    #[test]
    fn derivation_matches_previous_releases() {
        let kp = IdentitySeed([7; 32]).derive_identity();
        let legacy_ed = {
            let mut out = [0u8; 32];
            ::hkdf::Hkdf::<sha2::Sha256>::new(None, &[7; 32])
                .expand(b"cypher-ed25519", &mut out)
                .unwrap();
            out
        };
        assert_eq!(kp.signing_key.to_bytes(), legacy_ed);
    }
}
