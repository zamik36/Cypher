use std::fmt;

use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};

macro_rules! fixed_id {
    ($(#[$meta:meta])* $name:ident, $len:expr) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Hash, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
        pub struct $name(pub [u8; $len]);

        impl $name {
            pub const LEN: usize = $len;

            pub fn from_bytes(b: &[u8]) -> Option<Self> {
                b.try_into().ok().map(Self)
            }

            pub fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }

            pub fn to_vec(self) -> Vec<u8> {
                self.0.to_vec()
            }

            pub fn to_hex(self) -> String {
                hex::encode(self.0)
            }

            pub fn from_hex(s: &str) -> Option<Self> {
                let mut out = [0u8; $len];
                hex::decode_to_slice(s, &mut out).ok()?;
                Some(Self(out))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), hex::encode(&self.0[..4]))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&hex::encode(&self.0[..4]))
            }
        }
    };
}

fixed_id!(
    /// Long-term peer identity: the Ed25519 public key.
    PeerId,
    32
);

fixed_id!(
    /// Random identifier of a file or media transfer.
    FileId,
    16
);

fixed_id!(
    /// Random identifier of an end-to-end message, used for dedup and receipts.
    MsgId,
    16
);

impl FileId {
    pub fn random(rng: &mut impl CryptoRngCore) -> Self {
        let mut b = [0u8; 16];
        rng.fill_bytes(&mut b);
        Self(b)
    }
}

impl MsgId {
    pub fn random(rng: &mut impl CryptoRngCore) -> Self {
        let mut b = [0u8; 16];
        rng.fill_bytes(&mut b);
        Self(b)
    }
}

/// Share-link identifier: 128 random bits in lowercase RFC 4648 base32.
#[derive(Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinkId(String);

impl LinkId {
    pub const ENCODED_LEN: usize = 26;

    pub fn random(rng: &mut impl CryptoRngCore) -> Self {
        let mut b = [0u8; 16];
        rng.fill_bytes(&mut b);
        Self(base32_encode(&b))
    }

    /// Accepts only well-formed identifiers so untrusted input can never be
    /// used to build arbitrary storage keys.
    pub fn parse(s: &str) -> Option<Self> {
        let valid = s.len() == Self::ENCODED_LEN
            && s.bytes().all(|c| matches!(c, b'a'..=b'z' | b'2'..=b'7'));
        valid.then(|| Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LinkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn base32_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut bits = 0u32;
    let mut num_bits = 0u32;
    // Masked to five bits, so the lookup always hits the alphabet.
    let symbol = |v: u32| ALPHABET.get((v & 0x1F) as usize).map(|&c| char::from(c));
    for &byte in data {
        bits = (bits << 8) | u32::from(byte);
        num_bits += 8;
        while num_bits >= 5 {
            num_bits -= 5;
            out.extend(symbol(bits >> num_bits));
        }
    }
    if num_bits > 0 {
        out.extend(symbol(bits << (5 - num_bits)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_id_roundtrip_and_validation() {
        let id = LinkId::random(&mut rand::rngs::OsRng);
        assert_eq!(id.as_str().len(), LinkId::ENCODED_LEN);
        assert_eq!(LinkId::parse(id.as_str()), Some(id));
        assert!(LinkId::parse("short").is_none());
        assert!(LinkId::parse("ABCDEFGHIJKLMNOPQRSTUVWXYZ").is_none());
        assert!(LinkId::parse("abcdefghijklmnopqrstuvwxy1").is_none());
    }

    #[test]
    fn base32_known_vector() {
        assert_eq!(base32_encode(b"foobar"), "mzxw6ytboi");
    }

    #[test]
    fn peer_id_hex_roundtrip() {
        let id = PeerId([0xAB; 32]);
        assert_eq!(PeerId::from_hex(&id.to_hex()), Some(id));
        assert!(PeerId::from_hex("zz").is_none());
        assert!(PeerId::from_bytes(&[0u8; 31]).is_none());
    }
}
