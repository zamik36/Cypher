use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use p256::ecdsa::Signature;
use p256::ecdsa::signature::Signer as _;

use super::WebPush;

impl WebPush {
    /// `vapid t=<JWT>, k=<public key>` for `audience`.
    pub(super) fn authorization(&self, audience: &str) -> String {
        let exp = crate::now_secs() + 12 * 3600;
        self.authorization_until(audience, exp)
    }

    /// `vapid t=<JWT>, k=<public key>` for `audience`, valid until `exp`
    /// (seconds since the Unix epoch).
    fn authorization_until(&self, audience: &str, exp: u64) -> String {
        let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = B64.encode(format!(
            r#"{{"aud":"{audience}","exp":{exp},"sub":"{}"}}"#,
            self.contact
        ));
        let signed = format!("{header}.{claims}");
        let signature: Signature = self.key.sign(signed.as_bytes());
        format!(
            "vapid t={signed}.{}, k={}",
            B64.encode(signature.to_bytes()),
            B64.encode(self.public)
        )
    }
}

#[cfg(test)]
mod tests {
    use rand::RngCore;
    use rand::rngs::OsRng;

    use super::super::Policy;
    use super::*;

    fn b64(s: &str) -> Vec<u8> {
        B64.decode(s).unwrap()
    }

    /// VAPID keys and headers fixed by earlier releases: the public key
    /// clients subscribed with, and the header, are the same bytes, as
    /// ES256 signs deterministically (RFC 6979). The vectors were checked
    /// against OpenSSL.
    const VAPID_VECTORS: &[(&str, &str, &str, u64, &str, &str)] = &[
        (
            "BQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU",
            "mailto:ops@example.org",
            "https://fcm.googleapis.com",
            1_700_000_000,
            "BAeBDql0zqV3PmO4l_N-O-mgnnpf6blxpE0QZawqOpMRY35HpPmUZKD9zkSojsfXA6kYPB0GVS2eLWt2BpSBxXc",
            "eyJ0eXAiOiJKV1QiLCJhbGciOiJFUzI1NiJ9.eyJhdWQiOiJodHRwczovL2ZjbS5nb29nbGVhcGlzLmNvbSIsImV4cCI6MTcwMDAwMDAwMCwic3ViIjoibWFpbHRvOm9wc0BleGFtcGxlLm9yZyJ9.KX7Ba-BchzbCln7V8ZSerazrBU-FxRaEbczW64Ce-I2dvk0O66F7H2RDK0VoFIr-VTxoyZQ-LaKWbojcWmfHSQ",
        ),
        (
            "AQgPFh0kKzI5QEdOVVxjanF4f4aNlJuiqbC3vsXM09o",
            "https://cyphermessanger.tech",
            "https://updates.push.services.mozilla.com",
            1_800_000_000,
            "BOty342XcWXegEmVMqJoUwzWB3ddFg67WIgy_1GGl2VhR3QeGatV_mFSMjhrdCJLfvX63iB4CLbNE3Mc9Sc_ZAI",
            "eyJ0eXAiOiJKV1QiLCJhbGciOiJFUzI1NiJ9.eyJhdWQiOiJodHRwczovL3VwZGF0ZXMucHVzaC5zZXJ2aWNlcy5tb3ppbGxhLmNvbSIsImV4cCI6MTgwMDAwMDAwMCwic3ViIjoiaHR0cHM6Ly9jeXBoZXJtZXNzYW5nZXIudGVjaCJ9.jEqFDFNpwOII3VyvDID0CzU88yzxOtI1xYr4H9zhfKB3QqYTEn0z5DGppAnbVBV623ud06FqL_0Q5brvDz-2Ng",
        ),
    ];

    #[test]
    fn vapid_headers_match_the_known_answers() {
        use p256::ecdsa::VerifyingKey;
        use p256::ecdsa::signature::Verifier as _;

        for &(secret, contact, audience, exp, public, jwt) in VAPID_VECTORS {
            let secret: [u8; 32] = b64(secret).try_into().unwrap();
            let web = WebPush::new(secret, contact.into(), Policy::insecure_loopback()).unwrap();
            assert_eq!(B64.encode(web.public_key()), public);
            assert_eq!(
                web.authorization_until(audience, exp),
                format!("vapid t={jwt}, k={public}")
            );
            let (signed, signature) = jwt.rsplit_once('.').unwrap();
            let signature = Signature::from_slice(&b64(signature)).unwrap();
            VerifyingKey::from_sec1_bytes(&b64(public))
                .unwrap()
                .verify(signed.as_bytes(), &signature)
                .unwrap();
        }
    }

    /// A VAPID key file written by an earlier release still loads as the
    /// same key, and loading leaves the file as it was.
    #[test]
    fn a_stored_vapid_key_still_loads() {
        let &(secret, contact, _, _, public, _) = &VAPID_VECTORS[1];
        let dir = std::env::temp_dir().join(format!("cypher-vapid-{}", OsRng.next_u64()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vapid.key");
        std::fs::write(&path, b64(secret)).unwrap();

        let loaded = cypher_server_kit::secrets::load_or_create_secret(&path).unwrap();
        let web = WebPush::new(loaded, contact.into(), Policy::insecure_loopback()).unwrap();
        assert_eq!(B64.encode(web.public_key()), public);
        assert_eq!(std::fs::read(&path).unwrap(), b64(secret));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Zero and scalars not below the group order are refused, as before.
    #[test]
    fn invalid_vapid_keys_are_refused() {
        let order = b64("_____wAAAAD__________7zm-q2nF56E87nKwvxjJVE");
        let order: [u8; 32] = order.try_into().unwrap();
        for secret in [[0; 32], [0xff; 32], order] {
            let web = WebPush::new(secret, String::new(), Policy::insecure_loopback());
            assert!(web.is_err());
        }
    }
}
