use cypher_types::{DeviceId, PeerId};
use rand_core::CryptoRngCore;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::error::CryptoError;
use crate::identity::{IdentityKeyPair, verify_signature};

const SPK_SIGNATURE_CONTEXT: &[u8] = b"cypher/v3/spk";

/// Medium-term X25519 prekey, signed by the identity key and rotated periodically.
pub struct SignedPreKey {
    id: u32,
    secret: StaticSecret,
}

/// Single-use X25519 prekey; the server hands each one out at most once.
pub struct OneTimePreKey {
    id: u32,
    secret: StaticSecret,
}

macro_rules! prekey_common {
    ($t:ty) => {
        impl $t {
            pub fn generate(id: u32, rng: &mut impl CryptoRngCore) -> Self {
                Self {
                    id,
                    secret: StaticSecret::random_from_rng(rng),
                }
            }

            pub fn from_secret_bytes(id: u32, secret: [u8; 32]) -> Self {
                Self {
                    id,
                    secret: StaticSecret::from(secret),
                }
            }

            pub fn id(&self) -> u32 {
                self.id
            }

            pub fn secret_bytes(&self) -> Zeroizing<[u8; 32]> {
                Zeroizing::new(self.secret.to_bytes())
            }

            pub fn public(&self) -> [u8; 32] {
                PublicKey::from(&self.secret).to_bytes()
            }

            pub(crate) fn secret(&self) -> &StaticSecret {
                &self.secret
            }
        }
    };
}

prekey_common!(SignedPreKey);
prekey_common!(OneTimePreKey);

impl SignedPreKey {
    /// Signs this prekey as `device`'s: the server cannot serve one device's
    /// prekey under another's name.
    pub fn sign(&self, identity: &IdentityKeyPair, device: DeviceId) -> [u8; 64] {
        let msg = spk_signed_message(
            device,
            &identity.dh_public_key().to_bytes(),
            self.id,
            &self.public(),
        );
        identity.sign(&msg).to_bytes()
    }
}

fn spk_signed_message(
    device: DeviceId,
    identity_dh: &[u8; 32],
    spk_id: u32,
    spk: &[u8; 32],
) -> [u8; 85] {
    let mut msg = [0u8; 85];
    msg[..13].copy_from_slice(SPK_SIGNATURE_CONTEXT);
    msg[13..17].copy_from_slice(&device.0.to_le_bytes());
    msg[17..49].copy_from_slice(identity_dh);
    msg[49..53].copy_from_slice(&spk_id.to_le_bytes());
    msg[53..85].copy_from_slice(spk);
    msg
}

/// Public material one device of a peer publishes so others can start a
/// session with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrekeyBundle {
    pub identity: PeerId,
    pub device: DeviceId,
    pub identity_dh: [u8; 32],
    pub spk_id: u32,
    pub spk: [u8; 32],
    pub spk_signature: [u8; 64],
    pub opk: Option<(u32, [u8; 32])>,
}

impl PrekeyBundle {
    const BASE_LEN: usize = 32 + 4 + 32 + 4 + 32 + 64 + 1;
    pub const MAX_LEN: usize = Self::BASE_LEN + 4 + 32;

    pub fn new(
        identity: &IdentityKeyPair,
        device: DeviceId,
        spk: &SignedPreKey,
        opk: Option<&OneTimePreKey>,
    ) -> Self {
        Self {
            identity: identity.peer_id(),
            device,
            identity_dh: identity.dh_public_key().to_bytes(),
            spk_id: spk.id,
            spk: spk.public(),
            spk_signature: spk.sign(identity, device),
            opk: opk.map(|k| (k.id(), k.public())),
        }
    }

    /// Checks that the signed prekey (and the device and DH identity it is
    /// bound to) was signed by the Ed25519 identity that *is* the peer id.
    pub fn verify(&self) -> Result<(), CryptoError> {
        let msg = spk_signed_message(self.device, &self.identity_dh, self.spk_id, &self.spk);
        verify_signature(&self.identity, &msg, &self.spk_signature)
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.reserve(Self::MAX_LEN);
        out.extend_from_slice(self.identity.as_bytes());
        out.extend_from_slice(&self.device.0.to_le_bytes());
        out.extend_from_slice(&self.identity_dh);
        out.extend_from_slice(&self.spk_id.to_le_bytes());
        out.extend_from_slice(&self.spk);
        out.extend_from_slice(&self.spk_signature);
        match &self.opk {
            Some((id, key)) => {
                out.push(1);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(key);
            }
            None => out.push(0),
        }
    }

    /// Strict decoder: rejects trailing bytes so encodings are canonical.
    pub fn decode(b: &[u8]) -> Result<Self, CryptoError> {
        let mut r = crate::reader::Reader::new(b);
        let identity = PeerId(r.array()?);
        let device = DeviceId(r.u32()?);
        if !device.is_valid() {
            return Err(CryptoError::Malformed);
        }
        let identity_dh = r.array()?;
        let spk_id = r.u32()?;
        let spk = r.array()?;
        let spk_signature = r.array()?;
        let opk = match r.u8()? {
            0 => None,
            1 => Some((r.u32()?, r.array()?)),
            _ => return Err(CryptoError::Malformed),
        };
        r.finish()?;
        Ok(Self {
            identity,
            device,
            identity_dh,
            spk_id,
            spk,
            spk_signature,
            opk,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    fn bundle(with_opk: bool) -> PrekeyBundle {
        let ik = IdentityKeyPair::generate();
        let spk = SignedPreKey::generate(7, &mut OsRng);
        let opk = OneTimePreKey::generate(9, &mut OsRng);
        PrekeyBundle::new(&ik, DeviceId(3), &spk, with_opk.then_some(&opk))
    }

    #[test]
    fn bundle_roundtrip_and_verify() {
        for with_opk in [false, true] {
            let b = bundle(with_opk);
            let mut buf = Vec::new();
            b.encode(&mut buf);
            let decoded = PrekeyBundle::decode(&buf).unwrap();
            assert_eq!(decoded, b);
            decoded.verify().unwrap();
        }
    }

    #[test]
    fn tampered_bundle_is_rejected() {
        let mut b = bundle(false);
        b.spk[0] ^= 1;
        assert_eq!(b.verify(), Err(CryptoError::Signature));

        let mut b = bundle(false);
        b.identity_dh[0] ^= 1;
        assert_eq!(b.verify(), Err(CryptoError::Signature));

        let mut b = bundle(false);
        b.spk_id += 1;
        assert_eq!(b.verify(), Err(CryptoError::Signature));

        let mut b = bundle(false);
        b.device = DeviceId(4);
        assert_eq!(b.verify(), Err(CryptoError::Signature));
    }

    #[test]
    fn device_zero_is_not_a_bundle() {
        let mut buf = Vec::new();
        bundle(false).encode(&mut buf);
        buf[32..36].copy_from_slice(&[0; 4]);
        assert_eq!(PrekeyBundle::decode(&buf), Err(CryptoError::Malformed));
    }

    #[test]
    fn decode_rejects_trailing_and_truncated() {
        let mut buf = Vec::new();
        bundle(true).encode(&mut buf);
        buf.push(0);
        assert_eq!(PrekeyBundle::decode(&buf), Err(CryptoError::Malformed));
        buf.truncate(buf.len() - 2);
        assert_eq!(PrekeyBundle::decode(&buf), Err(CryptoError::Malformed));
    }
}
