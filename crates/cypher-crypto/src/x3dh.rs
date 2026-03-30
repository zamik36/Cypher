use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::PublicKey as X25519PublicKey;
use zeroize::Zeroize;

use x25519_dalek::StaticSecret as X25519StaticSecret;

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
/// shared secret regardless of which side calls it, because the cross-DH
/// outputs are sorted before concatenation.
///
/// Including both IK and SPK in the derivation provides forward secrecy:
/// compromising the long-term identity key alone does not reveal past sessions
/// (as long as the SPK was rotated).
///
/// Computes:
///   DH_ik  = X25519(our_ik_dh, their_ik_dh)           — symmetric
///   DH_a   = X25519(our_ik_dh, their_spk)             — cross term
///   DH_b   = X25519(our_spk_secret, their_ik_dh)      — cross term (counterpart)
///   SK     = HKDF-SHA256(DH_ik || sort(DH_a, DH_b))
pub fn x3dh_mutual(
    our_ik: &IdentityKeyPair,
    our_spk_secret: &X25519StaticSecret,
    their_ik_dh: &X25519PublicKey,
    their_spk: &X25519PublicKey,
) -> SharedSecret {
    // DH between identity keys — symmetric by construction.
    let dh_ik = our_ik.dh_secret.diffie_hellman(their_ik_dh);

    // Cross-terms: our_ik × their_spk  and  our_spk × their_ik.
    // By DH commutativity the other side computes the same two values
    // in swapped positions, so sorting ensures identical concatenation.
    let dh_cross_a = our_ik.dh_secret.diffie_hellman(their_spk);
    let dh_cross_b = our_spk_secret.diffie_hellman(their_ik_dh);

    let (first, second) = if dh_cross_a.as_bytes() <= dh_cross_b.as_bytes() {
        (dh_cross_a, dh_cross_b)
    } else {
        (dh_cross_b, dh_cross_a)
    };

    let mut dh_concat = Vec::with_capacity(96);
    dh_concat.extend_from_slice(dh_ik.as_bytes());
    dh_concat.extend_from_slice(first.as_bytes());
    dh_concat.extend_from_slice(second.as_bytes());

    kdf(&dh_concat)
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

        let alice_secret = x3dh_mutual(
            &alice_ik,
            &alice_spk.secret,
            &bob_ik.dh_public_key(),
            &bob_spk.public_key(),
        );
        let bob_secret = x3dh_mutual(
            &bob_ik,
            &bob_spk.secret,
            &alice_ik.dh_public_key(),
            &alice_spk.public_key(),
        );

        assert_eq!(alice_secret.0, bob_secret.0);
    }
}
