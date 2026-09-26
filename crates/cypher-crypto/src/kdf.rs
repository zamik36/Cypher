use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

type HmacSha256 = Hmac<Sha256>;

/// HKDF-SHA256 extract-and-expand into a fixed-size, auto-zeroizing buffer.
pub(crate) fn hkdf<const N: usize>(
    salt: Option<&[u8]>,
    ikm: &[u8],
    info: &[u8],
) -> Zeroizing<[u8; N]> {
    const { assert!(N <= 255 * 32, "HKDF-SHA256 output too long") };
    let mut out = Zeroizing::new([0u8; N]);
    Hkdf::<Sha256>::new(salt, ikm)
        .expand(info, out.as_mut())
        .expect("length checked at compile time");
    out
}

/// Symmetric-key ratchet step (Signal KDF_CK): returns `(next_chain_key, message_key)`.
pub(crate) fn kdf_ck(ck: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let keyed = HmacSha256::new_from_slice(ck).expect("HMAC accepts any key length");
    let mut mk_mac = keyed.clone();
    mk_mac.update(&[0x01]);
    let mut ck_mac = keyed;
    ck_mac.update(&[0x02]);
    (
        ck_mac.finalize().into_bytes().into(),
        mk_mac.finalize().into_bytes().into(),
    )
}

/// Root-key ratchet step (Signal KDF_RK): returns `(root_key, chain_key)`.
pub(crate) fn kdf_rk(rk: &[u8; 32], dh_out: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let okm = hkdf::<64>(Some(rk), dh_out, b"cypher/v2/rk");
    let mut root = [0u8; 32];
    let mut chain = [0u8; 32];
    root.copy_from_slice(&okm[..32]);
    chain.copy_from_slice(&okm[32..]);
    (root, chain)
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct MessageKeys {
    pub key: [u8; 32],
    pub nonce: [u8; 12],
}

/// Expands a one-shot message key into an AES-256 key and a GCM nonce.
pub(crate) fn message_keys(mk: &[u8; 32]) -> MessageKeys {
    let okm = hkdf::<44>(None, mk, b"cypher/v2/msg");
    let mut keys = MessageKeys {
        key: [0; 32],
        nonce: [0; 12],
    };
    keys.key.copy_from_slice(&okm[..32]);
    keys.nonce.copy_from_slice(&okm[32..]);
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_step_is_deterministic_and_separates_outputs() {
        let (ck1, mk1) = kdf_ck(&[7u8; 32]);
        let (ck2, mk2) = kdf_ck(&[7u8; 32]);
        assert_eq!((ck1, mk1), (ck2, mk2));
        assert_ne!(ck1, mk1);
    }

    #[test]
    fn root_step_separates_outputs() {
        let (rk, ck) = kdf_rk(&[1u8; 32], &[2u8; 32]);
        assert_ne!(rk, ck);
        assert_ne!(rk, [1u8; 32]);
    }
}
