use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::PublicKey as X25519PublicKey;
use zeroize::Zeroize;

use crate::identity::{EphemeralKeyPair, IdentityKeyPair, SignedPreKey};

/// A 32-byte shared secret derived from X3DH.
#[derive(Clone, Zeroize)]
#[zeroize(drop)]
pub struct SharedSecret(pub [u8; 32]);

impl SharedSecret {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Derive a shared secret from concatenated DH outputs using HKDF-SHA256.
fn kdf(dh_concat: &[u8]) -> SharedSecret {
    let hk = Hkdf::<Sha256>::new(Some(b"x3dh-salt"), dh_concat);
    let mut secret = [0u8; 32];
    hk.expand(b"x3dh-shared-secret", &mut secret)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    SharedSecret(secret)
}

/// X3DH initiator (Alice) side.
///
/// Computes:
///   DH1 = X25519(IK_A_dh, SPK_B)
///   DH2 = X25519(EK_A, IK_B_dh)
///   DH3 = X25519(EK_A, SPK_B)
///   SK  = HKDF-SHA256(DH1 || DH2 || DH3)
///
/// Parameters:
/// - `our_ik`: Our (Alice's) identity keypair
/// - `our_ek`: Our ephemeral keypair for this handshake
/// - `their_ik_dh`: Bob's X25519 identity DH public key
/// - `their_spk`: Bob's signed pre-key public key
pub fn x3dh_initiator(
    our_ik: &IdentityKeyPair,
    our_ek: &EphemeralKeyPair,
    their_ik_dh: &X25519PublicKey,
    their_spk: &X25519PublicKey,
) -> SharedSecret {
    let dh1 = our_ik.dh_secret.diffie_hellman(their_spk);
    let dh2 = our_ek.secret.diffie_hellman(their_ik_dh);
    let dh3 = our_ek.secret.diffie_hellman(their_spk);

    let mut dh_concat = Vec::with_capacity(96);
    dh_concat.extend_from_slice(dh1.as_bytes());
    dh_concat.extend_from_slice(dh2.as_bytes());
    dh_concat.extend_from_slice(dh3.as_bytes());

    kdf(&dh_concat)
}

/// X3DH responder (Bob) side.
///
/// Computes:
///   DH1 = X25519(SPK_B, IK_A_dh)
///   DH2 = X25519(IK_B_dh, EK_A)
///   DH3 = X25519(SPK_B, EK_A)
///   SK  = HKDF-SHA256(DH1 || DH2 || DH3)
///
/// Parameters:
/// - `our_ik`: Our (Bob's) identity keypair
/// - `our_spk`: Our signed pre-key
/// - `their_ik_dh`: Alice's X25519 identity DH public key
/// - `their_ek`: Alice's ephemeral public key
pub fn x3dh_responder(
    our_ik: &IdentityKeyPair,
    our_spk: &SignedPreKey,
    their_ik_dh: &X25519PublicKey,
    their_ek: &X25519PublicKey,
) -> SharedSecret {
    let dh1 = our_spk.secret.diffie_hellman(their_ik_dh);
    let dh2 = our_ik.dh_secret.diffie_hellman(their_ek);
    let dh3 = our_spk.secret.diffie_hellman(their_ek);

    let mut dh_concat = Vec::with_capacity(96);
    dh_concat.extend_from_slice(dh1.as_bytes());
    dh_concat.extend_from_slice(dh2.as_bytes());
    dh_concat.extend_from_slice(dh3.as_bytes());

    kdf(&dh_concat)
}

/// Symmetric key agreement for mutual session establishment.
///
/// Both peers call this with each other's public keys. The result is the same
/// shared secret regardless of which side calls it, because the DH outputs
/// are sorted before concatenation.
///
/// This is used when both peers independently fetch each other's prekeys and
/// need to derive the same shared secret without transmitting an ephemeral key.
///
/// Computes:
///   DH1 = X25519(our_ik_dh, their_spk)
///   DH2 = X25519(our_ik_dh, their_ik_dh)
///   SK  = HKDF-SHA256(sort(DH1, DH2))
pub fn x3dh_mutual(
    our_ik: &IdentityKeyPair,
    their_ik_dh: &X25519PublicKey,
    _their_spk: &X25519PublicKey,
) -> SharedSecret {
    // Symmetric ECDH: DH(our_ik, their_ik) produces the same shared secret
    // on both sides. Combined with HKDF for proper key derivation.
    let dh = our_ik.dh_secret.diffie_hellman(their_ik_dh);
    kdf(dh.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x3dh_initiator_responder_agree() {
        // Alice (initiator)
        let alice_ik = IdentityKeyPair::generate();
        let alice_ek = EphemeralKeyPair::generate();

        // Bob (responder)
        let bob_ik = IdentityKeyPair::generate();
        let bob_spk = SignedPreKey::generate();

        let alice_secret = x3dh_initiator(
            &alice_ik,
            &alice_ek,
            &bob_ik.dh_public_key(),
            &bob_spk.public_key(),
        );

        let bob_secret = x3dh_responder(
            &bob_ik,
            &bob_spk,
            &alice_ik.dh_public_key(),
            &alice_ek.public_key(),
        );

        assert_eq!(alice_secret.0, bob_secret.0);
    }

    #[test]
    fn x3dh_mutual_symmetric() {
        let alice_ik = IdentityKeyPair::generate();
        let alice_spk = SignedPreKey::generate();

        let bob_ik = IdentityKeyPair::generate();
        let bob_spk = SignedPreKey::generate();

        let alice_secret = x3dh_mutual(&alice_ik, &bob_ik.dh_public_key(), &bob_spk.public_key());
        let bob_secret = x3dh_mutual(&bob_ik, &alice_ik.dh_public_key(), &alice_spk.public_key());

        assert_eq!(alice_secret.0, bob_secret.0);
    }
}
