use std::sync::atomic::{AtomicU64, Ordering};

use cypher_crypto::identity::EphemeralKeyPair;
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::PublicKey as X25519PublicKey;
use zeroize::Zeroize;

/// A 1-hop onion circuit established with 0-RTT ephemeral key exchange.
///
/// The client generates a fresh ephemeral X25519 keypair, performs DH with the
/// relay's static public key, and derives a symmetric `circuit_key` used for
/// AEAD encryption of all messages on this circuit.
///
/// The relay derives the same key upon receiving the client's ephemeral public
/// key embedded in the first onion message — no extra round-trip needed.
///
/// Each message uses a unique nonce derived from `circuit_id || seq_no` to
/// prevent AES-GCM nonce reuse (which would be catastrophic for confidentiality).
pub struct Circuit {
    /// Unique random identifier for this circuit (16 bytes).
    pub circuit_id: [u8; 16],
    /// Symmetric AEAD key derived from the DH shared secret.
    pub circuit_key: [u8; 32],
    /// Ephemeral public key to embed in outgoing messages so the relay can
    /// derive the same shared secret.
    pub ephemeral_public: X25519PublicKey,
    /// Monotonic sequence counter for unique AEAD nonces.
    seq: AtomicU64,
}

impl Circuit {
    /// Create a new circuit by performing X25519 DH with the relay's static
    /// public key and deriving a symmetric key via HKDF-SHA256.
    pub fn new(relay_static_pk: &X25519PublicKey) -> Self {
        let ek = EphemeralKeyPair::generate();
        let ephemeral_public = ek.public_key();

        // X25519 DH
        let mut dh_shared = ek.secret.diffie_hellman(relay_static_pk).to_bytes();

        // KDF: HKDF-SHA256(salt="cypher-circuit-v1", IKM=dh_shared)
        let hk = Hkdf::<Sha256>::new(Some(b"cypher-circuit-v1"), &dh_shared);
        let mut circuit_key = [0u8; 32];
        hk.expand(b"circuit-key", &mut circuit_key)
            .expect("32 bytes is valid HKDF-SHA256 output");

        dh_shared.zeroize();

        let circuit_id: [u8; 16] = rand::random();

        Self {
            circuit_id,
            circuit_key,
            ephemeral_public,
            seq: AtomicU64::new(0),
        }
    }

    /// Derive circuit key from a known ephemeral public key and the relay's
    /// static secret. Used by the **relay side** to reconstruct the same key.
    pub fn derive_relay_side(
        relay_static_secret: &x25519_dalek::StaticSecret,
        client_ephemeral_pk: &X25519PublicKey,
        circuit_id: [u8; 16],
    ) -> Self {
        let mut dh_shared = relay_static_secret
            .diffie_hellman(client_ephemeral_pk)
            .to_bytes();

        let hk = Hkdf::<Sha256>::new(Some(b"cypher-circuit-v1"), &dh_shared);
        let mut circuit_key = [0u8; 32];
        hk.expand(b"circuit-key", &mut circuit_key)
            .expect("32 bytes is valid HKDF-SHA256 output");

        dh_shared.zeroize();

        Self {
            circuit_id,
            circuit_key,
            ephemeral_public: *client_ephemeral_pk,
            seq: AtomicU64::new(0),
        }
    }

    /// Get the next unique nonce material for AEAD encryption.
    ///
    /// Returns `circuit_id (16B) || seq_no (8B LE)` = 24 bytes.
    /// Each call increments the sequence counter, guaranteeing a unique nonce
    /// per message on this circuit.
    pub fn next_nonce_material(&self) -> [u8; 24] {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let mut material = [0u8; 24];
        material[..16].copy_from_slice(&self.circuit_id);
        material[16..].copy_from_slice(&seq.to_le_bytes());
        material
    }
}

impl Drop for Circuit {
    fn drop(&mut self) {
        self.circuit_key.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x25519_dalek::StaticSecret;

    #[test]
    fn client_relay_derive_same_key() {
        let relay_secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let relay_pk = X25519PublicKey::from(&relay_secret);

        let client_circuit = Circuit::new(&relay_pk);

        let relay_circuit = Circuit::derive_relay_side(
            &relay_secret,
            &client_circuit.ephemeral_public,
            client_circuit.circuit_id,
        );

        assert_eq!(client_circuit.circuit_key, relay_circuit.circuit_key);
        assert_eq!(client_circuit.circuit_id, relay_circuit.circuit_id);
    }

    #[test]
    fn different_sessions_different_keys() {
        let relay_secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let relay_pk = X25519PublicKey::from(&relay_secret);

        let c1 = Circuit::new(&relay_pk);
        let c2 = Circuit::new(&relay_pk);

        assert_ne!(c1.circuit_key, c2.circuit_key);
        assert_ne!(c1.circuit_id, c2.circuit_id);
    }
}
