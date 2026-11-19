//! X3DH key agreement (Signal spec) producing a ready-to-use [`Ratchet`].
//!
//! The initiator is always the party that joined via a share link. It sends
//! [`InitHeader`] alongside its first ratchet message so the responder can
//! derive the same session without a round trip. The header is signed by the
//! initiator's Ed25519 identity, binding its X25519 identity key to its peer id
//! and the session to the two devices it joins.

use cypher_types::{DeviceId, PeerId};
use rand_core::CryptoRngCore;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::double_ratchet::{Ratchet, dh};
use crate::error::CryptoError;
use crate::identity::{IdentityKeyPair, verify_signature};
use crate::kdf::hkdf;
use crate::prekey::{OneTimePreKey, PrekeyBundle, SignedPreKey};
use crate::reader::Reader;

const INIT_SIGNATURE_CONTEXT: &[u8] = b"cypher/v3/init";

/// Key-agreement parameters the initiator attaches to its first message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitHeader {
    pub identity_dh: [u8; 32],
    pub ephemeral: [u8; 32],
    pub spk_id: u32,
    pub opk_id: Option<u32>,
    pub signature: [u8; 64],
}

impl InitHeader {
    pub const MAX_LEN: usize = 32 + 32 + 4 + 1 + 4 + 64;

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.identity_dh);
        out.extend_from_slice(&self.ephemeral);
        out.extend_from_slice(&self.spk_id.to_le_bytes());
        match self.opk_id {
            Some(id) => {
                out.push(1);
                out.extend_from_slice(&id.to_le_bytes());
            }
            None => out.push(0),
        }
        out.extend_from_slice(&self.signature);
    }

    /// Decodes a header prefix and returns the remaining bytes.
    pub fn decode_prefix(b: &[u8]) -> Result<(Self, &[u8]), CryptoError> {
        let mut r = Reader::new(b);
        let identity_dh = r.array()?;
        let ephemeral = r.array()?;
        let spk_id = r.u32()?;
        let opk_id = match r.u8()? {
            0 => None,
            1 => Some(r.u32()?),
            _ => return Err(CryptoError::Malformed),
        };
        let signature = r.array()?;
        let header = Self {
            identity_dh,
            ephemeral,
            spk_id,
            opk_id,
            signature,
        };
        Ok((header, r.rest()))
    }

    /// `context ‖ identity_dh ‖ ephemeral ‖ initiator_device ‖ responder ‖
    /// responder_device ‖ spk_id ‖ has_opk ‖ opk_id`.
    fn signed_message(
        &self,
        initiator_device: DeviceId,
        responder: &PeerId,
        responder_device: DeviceId,
    ) -> Vec<u8> {
        let (has_opk, opk_id) = match self.opk_id {
            Some(id) => (1u8, id.to_le_bytes()),
            None => (0, [0; 4]),
        };
        [
            INIT_SIGNATURE_CONTEXT,
            &self.identity_dh,
            &self.ephemeral,
            &initiator_device.0.to_le_bytes(),
            responder.as_bytes(),
            &responder_device.0.to_le_bytes(),
            &self.spk_id.to_le_bytes(),
            &[has_opk],
            &opk_id,
        ]
        .concat()
    }
}

/// Starts a session from our `device` with the device that published
/// `bundle`.
pub fn initiate(
    ours: &IdentityKeyPair,
    device: DeviceId,
    bundle: &PrekeyBundle,
    rng: &mut impl CryptoRngCore,
) -> Result<(Ratchet, InitHeader), CryptoError> {
    bundle.verify()?;
    let ek = StaticSecret::random_from_rng(&mut *rng);

    let mut ikm = Zeroizing::new(Vec::with_capacity(32 * 5));
    ikm.extend_from_slice(&[0xFF; 32]);
    ikm.extend_from_slice(&*dh(&ours.dh_secret, &bundle.spk)?);
    ikm.extend_from_slice(&*dh(&ek, &bundle.identity_dh)?);
    ikm.extend_from_slice(&*dh(&ek, &bundle.spk)?);
    if let Some((_, opk)) = &bundle.opk {
        ikm.extend_from_slice(&*dh(&ek, opk)?);
    }

    let sk = shared_key(&ikm);
    let ad = [ours.peer_id().0, bundle.identity.0];
    let ratchet = Ratchet::init_initiator(&sk, ad, &bundle.spk, rng)?;
    let mut header = InitHeader {
        identity_dh: ours.dh_public_key().to_bytes(),
        ephemeral: PublicKey::from(&ek).to_bytes(),
        spk_id: bundle.spk_id,
        opk_id: bundle.opk.map(|(id, _)| id),
        signature: [0; 64],
    };
    header.signature = ours
        .sign(&header.signed_message(device, &bundle.identity, bundle.device))
        .to_bytes();
    Ok((ratchet, header))
}

/// Accepts on our `device` a session started by `initiator`'s
/// `initiator_device`. The caller resolves `spk` and `opk` from
/// `header.spk_id` / `header.opk_id` and must delete the one-time prekey
/// afterwards.
pub fn respond(
    ours: &IdentityKeyPair,
    device: DeviceId,
    spk: &SignedPreKey,
    opk: Option<&OneTimePreKey>,
    initiator: (&PeerId, DeviceId),
    header: &InitHeader,
) -> Result<Ratchet, CryptoError> {
    let (initiator, initiator_device) = initiator;
    let opk_matches = match (header.opk_id, opk) {
        (None, None) => true,
        (Some(id), Some(k)) => id == k.id(),
        _ => false,
    };
    if spk.id() != header.spk_id || !opk_matches {
        return Err(CryptoError::Malformed);
    }
    verify_signature(
        initiator,
        &header.signed_message(initiator_device, &ours.peer_id(), device),
        &header.signature,
    )?;

    let mut ikm = Zeroizing::new(Vec::with_capacity(32 * 5));
    ikm.extend_from_slice(&[0xFF; 32]);
    ikm.extend_from_slice(&*dh(spk.secret(), &header.identity_dh)?);
    ikm.extend_from_slice(&*dh(&ours.dh_secret, &header.ephemeral)?);
    ikm.extend_from_slice(&*dh(spk.secret(), &header.ephemeral)?);
    if let Some(opk) = opk {
        ikm.extend_from_slice(&*dh(opk.secret(), &header.ephemeral)?);
    }

    let sk = shared_key(&ikm);
    let ad = [initiator.0, ours.peer_id().0];
    Ok(Ratchet::init_responder(
        &sk,
        ad,
        StaticSecret::from(*spk.secret_bytes()),
    ))
}

fn shared_key(ikm: &[u8]) -> Zeroizing<[u8; 32]> {
    hkdf::<32>(Some(&[0u8; 32]), ikm, b"cypher/v3/x3dh")
}
