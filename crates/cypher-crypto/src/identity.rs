use cypher_common::{Error, PeerId, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::Sha256;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// A 256-bit seed from which all identity keys are deterministically derived.
///
/// The seed can be exported as a BIP39 mnemonic (24 words) for backup/restore.
/// On disk it is stored encrypted with a user-chosen passphrase via Argon2id + AES-256-GCM.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct IdentitySeed(pub [u8; 32]);

impl IdentitySeed {
    /// Generate a new random 256-bit seed.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    /// Encode the seed as a 24-word BIP39 mnemonic.
    pub fn to_mnemonic(&self) -> String {
        let m = bip39::Mnemonic::from_entropy(&self.0)
            .expect("32 bytes is valid entropy for BIP39");
        m.to_string()
    }

    /// Decode a 24-word BIP39 mnemonic back into a seed.
    pub fn from_mnemonic(phrase: &str) -> Result<Self> {
        let m = bip39::Mnemonic::parse(phrase)
            .map_err(|e| Error::Crypto(format!("invalid mnemonic: {e}")))?;
        let entropy = m.to_entropy();
        if entropy.len() != 32 {
            return Err(Error::Crypto(format!(
                "expected 32-byte entropy, got {}",
                entropy.len()
            )));
        }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&entropy);
        Ok(Self(bytes))
    }

    /// Deterministically derive an [`IdentityKeyPair`] from this seed.
    pub fn derive_identity(&self) -> IdentityKeyPair {
        IdentityKeyPair::from_seed(self)
    }

    /// Derive a Storage Encryption Key (SEK) for encrypting local data at rest.
    pub fn derive_storage_key(&self) -> [u8; 32] {
        let hk = Hkdf::<Sha256>::new(None, &self.0);
        let mut sek = [0u8; 32];
        hk.expand(b"cypher-storage-key", &mut sek)
            .expect("32 bytes is valid for HKDF-SHA256");
        sek
    }

    /// Return the raw seed bytes (for encryption/storage purposes).
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// An identity keypair containing both Ed25519 (for signing) and X25519 (for DH).
///
/// The Ed25519 key is used for signatures and deriving the PeerId.
/// The X25519 key is a separate static key used for X3DH key agreement.
pub struct IdentityKeyPair {
    /// Ed25519 signing key.
    pub signing_key: SigningKey,
    /// X25519 static secret for Diffie-Hellman.
    pub dh_secret: X25519StaticSecret,
}

impl IdentityKeyPair {
    /// Generate a new random identity keypair (ephemeral, not persistent).
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let dh_secret = X25519StaticSecret::random_from_rng(OsRng);
        Self {
            signing_key,
            dh_secret,
        }
    }

    /// Deterministically derive an identity keypair from a seed via HKDF-SHA256.
    pub fn from_seed(seed: &IdentitySeed) -> Self {
        let hk_ed = Hkdf::<Sha256>::new(None, &seed.0);
        let mut ed_bytes = [0u8; 32];
        hk_ed
            .expand(b"cypher-ed25519", &mut ed_bytes)
            .expect("32 bytes is valid for HKDF-SHA256");
        let signing_key = SigningKey::from_bytes(&ed_bytes);
        ed_bytes.zeroize();

        let hk_dh = Hkdf::<Sha256>::new(None, &seed.0);
        let mut dh_bytes = [0u8; 32];
        hk_dh
            .expand(b"cypher-x25519", &mut dh_bytes)
            .expect("32 bytes is valid for HKDF-SHA256");
        let dh_secret = X25519StaticSecret::from(dh_bytes);
        dh_bytes.zeroize();

        Self {
            signing_key,
            dh_secret,
        }
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    pub fn dh_public_key(&self) -> X25519PublicKey {
        X25519PublicKey::from(&self.dh_secret)
    }

    /// Derive a PeerId from the Ed25519 public key bytes.
    pub fn peer_id(&self) -> PeerId {
        PeerId(self.verifying_key().to_bytes())
    }

    /// Sign arbitrary data with the Ed25519 signing key.
    pub fn sign(&self, data: &[u8]) -> Signature {
        self.signing_key.sign(data)
    }

    /// Verify a signature against the Ed25519 verifying key.
    pub fn verify(&self, data: &[u8], signature: &Signature) -> Result<()> {
        self.verifying_key()
            .verify(data, signature)
            .map_err(|e| Error::Crypto(format!("Signature verification failed: {}", e)))
    }

    /// Verify a signature given a raw verifying key.
    pub fn verify_with_key(
        verifying_key: &VerifyingKey,
        data: &[u8],
        signature: &Signature,
    ) -> Result<()> {
        verifying_key
            .verify(data, signature)
            .map_err(|e| Error::Crypto(format!("Signature verification failed: {}", e)))
    }
}

/// A signed pre-key wrapping an X25519 static secret.
///
/// Used in X3DH as the semi-static key (SPK) that is signed by the identity key.
pub struct SignedPreKey {
    /// X25519 static secret.
    pub secret: X25519StaticSecret,
}

impl SignedPreKey {
    /// Generate a new random signed pre-key.
    pub fn generate() -> Self {
        Self {
            secret: X25519StaticSecret::random_from_rng(OsRng),
        }
    }

    pub fn public_key(&self) -> X25519PublicKey {
        X25519PublicKey::from(&self.secret)
    }
}

/// An ephemeral X25519 keypair used for a single X3DH handshake.
pub struct EphemeralKeyPair {
    /// X25519 ephemeral secret.
    pub secret: X25519StaticSecret,
}

impl EphemeralKeyPair {
    /// Generate a new random ephemeral keypair.
    pub fn generate() -> Self {
        Self {
            secret: X25519StaticSecret::random_from_rng(OsRng),
        }
    }

    pub fn public_key(&self) -> X25519PublicKey {
        X25519PublicKey::from(&self.secret)
    }
}

/// A key bundle published by a peer for X3DH key agreement.
///
/// Contains the peer's identity public keys and signed pre-key public key.
#[derive(Clone, Debug)]
pub struct KeyBundle {
    /// Ed25519 verifying key (for signature verification).
    pub identity_key: VerifyingKey,
    /// X25519 identity public key (for DH).
    pub identity_dh_key: X25519PublicKey,
    /// X25519 signed pre-key public key.
    pub signed_prekey: X25519PublicKey,
    /// Signature over the signed pre-key by the identity signing key.
    pub prekey_signature: Signature,
}

/// Wire size of a serialised [`KeyBundle`]: 32 + 32 + 32 + 64 = 160 bytes.
pub const KEY_BUNDLE_BYTES: usize = 160;

impl KeyBundle {
    /// Serialize to exactly [`KEY_BUNDLE_BYTES`] bytes.
    ///
    /// Layout: `identity_key(32) | identity_dh_key(32) | signed_prekey(32) | prekey_signature(64)`.
    pub fn to_bytes(&self) -> [u8; KEY_BUNDLE_BYTES] {
        let mut out = [0u8; KEY_BUNDLE_BYTES];
        out[0..32].copy_from_slice(self.identity_key.as_bytes());
        out[32..64].copy_from_slice(self.identity_dh_key.as_bytes());
        out[64..96].copy_from_slice(self.signed_prekey.as_bytes());
        out[96..160].copy_from_slice(&self.prekey_signature.to_bytes());
        out
    }

    /// Deserialize from at least [`KEY_BUNDLE_BYTES`] bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() < KEY_BUNDLE_BYTES {
            return Err(Error::Crypto(format!(
                "KeyBundle too short: {} < {}",
                data.len(),
                KEY_BUNDLE_BYTES
            )));
        }
        let ik_bytes: [u8; 32] = data[0..32].try_into().unwrap();
        let ik_dh_bytes: [u8; 32] = data[32..64].try_into().unwrap();
        let spk_bytes: [u8; 32] = data[64..96].try_into().unwrap();
        let sig_bytes: [u8; 64] = data[96..160].try_into().unwrap();

        let identity_key = VerifyingKey::from_bytes(&ik_bytes)
            .map_err(|e| Error::Crypto(format!("invalid identity key: {e}")))?;
        let identity_dh_key = X25519PublicKey::from(ik_dh_bytes);
        let signed_prekey = X25519PublicKey::from(spk_bytes);
        let prekey_signature = Signature::from_bytes(&sig_bytes);

        Ok(Self {
            identity_key,
            identity_dh_key,
            signed_prekey,
            prekey_signature,
        })
    }

    pub fn new(identity: &IdentityKeyPair, spk: &SignedPreKey) -> Self {
        let spk_pub = spk.public_key();
        let prekey_signature = identity.sign(spk_pub.as_bytes());
        Self {
            identity_key: identity.verifying_key(),
            identity_dh_key: identity.dh_public_key(),
            signed_prekey: spk_pub,
            prekey_signature,
        }
    }

    /// Verify that the signed pre-key was signed by the identity key.
    pub fn verify(&self) -> Result<()> {
        IdentityKeyPair::verify_with_key(
            &self.identity_key,
            self.signed_prekey.as_bytes(),
            &self.prekey_signature,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_sign_verify() {
        let kp = IdentityKeyPair::generate();
        let data = b"hello world";
        let sig = kp.sign(data);
        assert!(kp.verify(data, &sig).is_ok());
    }

    #[test]
    fn wrong_data_fails_verify() {
        let kp = IdentityKeyPair::generate();
        let sig = kp.sign(b"hello");
        assert!(kp.verify(b"world", &sig).is_err());
    }

    #[test]
    fn key_bundle_verify() {
        let identity = IdentityKeyPair::generate();
        let spk = SignedPreKey::generate();
        let bundle = KeyBundle::new(&identity, &spk);
        assert!(bundle.verify().is_ok());
    }

    #[test]
    fn key_bundle_roundtrip() {
        let identity = IdentityKeyPair::generate();
        let spk = SignedPreKey::generate();
        let bundle = KeyBundle::new(&identity, &spk);
        let bytes = bundle.to_bytes();
        assert_eq!(bytes.len(), KEY_BUNDLE_BYTES);
        let restored = KeyBundle::from_bytes(&bytes).unwrap();
        assert_eq!(
            bundle.identity_key.as_bytes(),
            restored.identity_key.as_bytes()
        );
        assert_eq!(
            bundle.identity_dh_key.as_bytes(),
            restored.identity_dh_key.as_bytes()
        );
        assert_eq!(
            bundle.signed_prekey.as_bytes(),
            restored.signed_prekey.as_bytes()
        );
        assert_eq!(
            bundle.prekey_signature.to_bytes(),
            restored.prekey_signature.to_bytes()
        );
        // Verify the signature still checks out after roundtrip.
        assert!(restored.verify().is_ok());
    }

    #[test]
    fn peer_id_from_identity() {
        let kp = IdentityKeyPair::generate();
        let peer_id = kp.peer_id();
        assert_eq!(peer_id.as_bytes(), &kp.verifying_key().to_bytes());
    }

    #[test]
    fn seed_deterministic_derivation() {
        let seed = IdentitySeed::generate();
        let kp1 = seed.derive_identity();
        let kp2 = seed.derive_identity();
        assert_eq!(kp1.peer_id().as_bytes(), kp2.peer_id().as_bytes());
        assert_eq!(kp1.dh_public_key().as_bytes(), kp2.dh_public_key().as_bytes());
    }

    #[test]
    fn seed_mnemonic_roundtrip() {
        let seed = IdentitySeed::generate();
        let mnemonic = seed.to_mnemonic();
        let words: Vec<&str> = mnemonic.split_whitespace().collect();
        assert_eq!(words.len(), 24);

        let restored = IdentitySeed::from_mnemonic(&mnemonic).unwrap();
        assert_eq!(seed.0, restored.0);

        // Derived identity must match.
        let kp1 = seed.derive_identity();
        let kp2 = restored.derive_identity();
        assert_eq!(kp1.peer_id().as_bytes(), kp2.peer_id().as_bytes());
    }

    #[test]
    fn seed_storage_key_deterministic() {
        let seed = IdentitySeed::generate();
        let sek1 = seed.derive_storage_key();
        let sek2 = seed.derive_storage_key();
        assert_eq!(sek1, sek2);
        // SEK must differ from signing key bytes.
        let kp = seed.derive_identity();
        assert_ne!(&sek1, kp.verifying_key().as_bytes());
    }

    #[test]
    fn different_seeds_different_identities() {
        let seed1 = IdentitySeed::generate();
        let seed2 = IdentitySeed::generate();
        let kp1 = seed1.derive_identity();
        let kp2 = seed2.derive_identity();
        assert_ne!(kp1.peer_id().as_bytes(), kp2.peer_id().as_bytes());
    }

    #[test]
    fn invalid_mnemonic_fails() {
        assert!(IdentitySeed::from_mnemonic("not a valid mnemonic").is_err());
    }
}
