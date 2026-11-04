use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use hkdf::Hkdf;
use p256::PublicKey;
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::Sha256;

use super::encoded;

/// Record size advertised in the aes128gcm header.
const RECORD_SIZE: u32 = 4096;

/// `payload` encrypted to a subscription's keys (RFC 8291, aes128gcm).
pub(super) fn encrypt(payload: &[u8], p256dh: &[u8; 65], auth: &[u8; 16]) -> Option<Vec<u8>> {
    let mut secret = [0u8; 32];
    let mut salt = [0u8; 16];
    loop {
        OsRng.fill_bytes(&mut secret);
        if p256::SecretKey::from_bytes(&secret.into()).is_ok() {
            break;
        }
    }
    OsRng.fill_bytes(&mut salt);
    encrypt_with(payload, p256dh, auth, &secret, &salt)
}

fn encrypt_with(
    payload: &[u8],
    p256dh: &[u8; 65],
    auth: &[u8; 16],
    sender_secret: &[u8; 32],
    salt: &[u8; 16],
) -> Option<Vec<u8>> {
    let receiver = PublicKey::from_sec1_bytes(p256dh).ok()?;
    let sender = p256::SecretKey::from_bytes(&(*sender_secret).into()).ok()?;
    let sender_public = encoded(&sender.public_key());
    let shared = p256::ecdh::diffie_hellman(sender.to_nonzero_scalar(), receiver.as_affine());

    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(p256dh);
    key_info.extend_from_slice(&sender_public);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), shared.raw_secret_bytes())
        .expand(&key_info, &mut ikm)
        .ok()?;
    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek)
        .ok()?;
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).ok()?;

    let mut record = payload.to_vec();
    record.push(2); // last (and only) record, no padding
    let ciphertext = Aes128Gcm::new(&cek.into())
        .encrypt(Nonce::from_slice(&nonce), record.as_slice())
        .ok()?;

    let mut body = Vec::with_capacity(16 + 4 + 1 + 65 + ciphertext.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(65);
    body.extend_from_slice(&sender_public);
    body.extend_from_slice(&ciphertext);
    Some(body)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;

    use super::super::PAYLOAD;
    use super::*;

    fn b64(s: &str) -> Vec<u8> {
        B64.decode(s).unwrap()
    }

    const PUBLIC: &str =
        "BAeBDql0zqV3PmO4l_N-O-mgnnpf6blxpE0QZawqOpMRY35HpPmUZKD9zkSojsfXA6kYPB0GVS2eLWt2BpSBxXc";

    /// A subscription key is an uncompressed point on the curve, nothing
    /// else: the hybrid form, a point off the curve and junk are refused.
    #[test]
    fn subscription_keys_must_be_uncompressed_points_on_the_curve() {
        let public: [u8; 65] = b64(PUBLIC).try_into().unwrap();
        let accepts = |key: &[u8; 65]| {
            let parsed = PublicKey::from_sec1_bytes(key).is_ok();
            assert_eq!(parsed, encrypt(PAYLOAD, key, &[1; 16]).is_some());
            parsed
        };
        assert!(accepts(&public));
        let mut off_curve = public;
        off_curve[64] ^= 1;
        let mut hybrid = public;
        hybrid[0] = 6 | (public[64] & 1);
        let mut compressed_tag = public;
        compressed_tag[0] = 2;
        for bad in [
            off_curve,
            hybrid,
            compressed_tag,
            [0; 65],
            [4; 65],
            [0xff; 65],
        ] {
            assert!(!accepts(&bad), "{}", B64.encode(bad));
        }
    }

    /// The worked example of RFC 8291, section 5.
    #[test]
    fn encrypts_like_the_rfc_example() {
        let p256dh: [u8; 65] = b64(
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
        )
        .try_into()
        .unwrap();
        let auth: [u8; 16] = b64("BTBZMqHH6r4Tts7J_aSIgg").try_into().unwrap();
        let sender: [u8; 32] = b64("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")
            .try_into()
            .unwrap();
        let salt: [u8; 16] = b64("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let body = encrypt_with(
            b"When I grow up, I want to be a watermelon",
            &p256dh,
            &auth,
            &sender,
            &salt,
        )
        .unwrap();
        assert_eq!(
            B64.encode(body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }
}
