//! The devices of one identity, as the identity itself lists them. The list
//! is signed and versioned so that the server, which stores and serves it,
//! can neither add a device nor hand out an older list in place of a newer
//! one to anybody who has already seen the newer. It does not protect
//! against a device holding the identity: that device can sign any list.

use cypher_types::{DeviceId, MAX_DEVICES, PeerId};

use crate::error::CryptoError;
use crate::identity::{IdentityKeyPair, verify_signature};
use crate::reader::Reader;

const DEVICES_SIGNATURE_CONTEXT: &[u8] = b"cypher/v3/devices";

/// A signed list of an identity's devices: sorted, unique, valid ids, at
/// least one and at most [`MAX_DEVICES`]. Only ever built by signing or by
/// decoding a correctly signed encoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceList {
    identity: PeerId,
    version: u64,
    devices: Vec<DeviceId>,
    signature: [u8; 64],
}

impl DeviceList {
    /// Longest encoding: identity, version, count, ids, signature.
    pub const MAX_LEN: usize = 32 + 8 + 1 + 4 * MAX_DEVICES + 64;

    /// Signs `devices` as `identity`'s list at `version`.
    pub fn sign(
        identity: &IdentityKeyPair,
        version: u64,
        devices: impl IntoIterator<Item = DeviceId>,
    ) -> Result<Self, CryptoError> {
        let mut devices: Vec<DeviceId> = devices.into_iter().collect();
        devices.sort_unstable();
        devices.dedup();
        let mut list = Self {
            identity: identity.peer_id(),
            version,
            devices,
            signature: [0; 64],
        };
        list.check_devices()?;
        list.signature = identity.sign(&list.signed_message()).to_bytes();
        Ok(list)
    }

    /// The next version of this list with `device` added.
    pub fn with(&self, identity: &IdentityKeyPair, device: DeviceId) -> Result<Self, CryptoError> {
        let devices = self.devices.iter().copied().chain([device]);
        Self::sign(identity, self.next_version()?, devices)
    }

    /// The next version of this list with `device` gone.
    pub fn without(
        &self,
        identity: &IdentityKeyPair,
        device: DeviceId,
    ) -> Result<Self, CryptoError> {
        let devices = self.devices.iter().copied().filter(|d| *d != device);
        Self::sign(identity, self.next_version()?, devices)
    }

    pub fn identity(&self) -> PeerId {
        self.identity
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn devices(&self) -> &[DeviceId] {
        &self.devices
    }

    pub fn contains(&self, device: DeviceId) -> bool {
        self.devices.binary_search(&device).is_ok()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.signed_body();
        out.extend_from_slice(&self.signature);
        out
    }

    /// Strict decoder that also checks the signature: a list that decodes
    /// is one its identity signed.
    pub fn decode(bytes: &[u8]) -> Result<Self, CryptoError> {
        let mut r = Reader::new(bytes);
        let identity = PeerId(r.array()?);
        let version = r.u64()?;
        let count = usize::from(r.u8()?);
        if count > MAX_DEVICES {
            return Err(CryptoError::Malformed);
        }
        let devices = (0..count)
            .map(|_| r.u32().map(DeviceId))
            .collect::<Result<Vec<_>, _>>()?;
        let signature = r.array()?;
        r.finish()?;
        let list = Self {
            identity,
            version,
            devices,
            signature,
        };
        let canonical = list.devices.is_sorted_by(|a, b| a < b);
        if !canonical {
            return Err(CryptoError::Malformed);
        }
        list.check_devices()?;
        verify_signature(&list.identity, &list.signed_message(), &list.signature)?;
        Ok(list)
    }

    fn next_version(&self) -> Result<u64, CryptoError> {
        self.version.checked_add(1).ok_or(CryptoError::Malformed)
    }

    fn check_devices(&self) -> Result<(), CryptoError> {
        let sized = (1..=MAX_DEVICES).contains(&self.devices.len());
        if sized && self.devices.iter().all(|d| d.is_valid()) {
            Ok(())
        } else {
            Err(CryptoError::Malformed)
        }
    }

    fn signed_body(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::MAX_LEN);
        out.extend_from_slice(self.identity.as_bytes());
        out.extend_from_slice(&self.version.to_le_bytes());
        // At most MAX_DEVICES, checked on every way in.
        out.push(u8::try_from(self.devices.len()).unwrap_or(u8::MAX));
        for device in &self.devices {
            out.extend_from_slice(&device.0.to_le_bytes());
        }
        out
    }

    fn signed_message(&self) -> Vec<u8> {
        [DEVICES_SIGNATURE_CONTEXT, &self.signed_body()].concat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &DeviceList) -> Vec<u32> {
        list.devices().iter().map(|d| d.0).collect()
    }

    #[test]
    fn a_list_round_trips_and_grows_and_shrinks_by_version() {
        let me = IdentityKeyPair::generate();
        let list = DeviceList::sign(&me, 1, [DeviceId(9), DeviceId(1), DeviceId(9)]).unwrap();
        assert_eq!(ids(&list), [1, 9]);
        assert_eq!(DeviceList::decode(&list.encode()).unwrap(), list);
        assert_eq!(list.identity(), me.peer_id());
        assert!(list.contains(DeviceId(9)) && !list.contains(DeviceId(2)));

        let grown = list.with(&me, DeviceId(4)).unwrap();
        assert_eq!((grown.version(), ids(&grown)), (2, vec![1, 4, 9]));
        let shrunk = grown.without(&me, DeviceId(1)).unwrap();
        assert_eq!((shrunk.version(), ids(&shrunk)), (3, vec![4, 9]));
        assert_eq!(DeviceList::decode(&shrunk.encode()).unwrap(), shrunk);
    }

    #[test]
    fn invalid_lists_cannot_be_signed() {
        let me = IdentityKeyPair::generate();
        DeviceList::sign(&me, 1, []).unwrap_err();
        DeviceList::sign(&me, 1, [DeviceId(0)]).unwrap_err();
        let full = DeviceList::sign(&me, 1, (1..=6).map(DeviceId)).unwrap();
        full.with(&me, DeviceId(7)).unwrap_err();
        let only = DeviceList::sign(&me, 1, [DeviceId(1)]).unwrap();
        only.without(&me, DeviceId(1)).unwrap_err();
        let last = DeviceList::sign(&me, u64::MAX, [DeviceId(1)]).unwrap();
        last.with(&me, DeviceId(2)).unwrap_err();
    }

    #[test]
    fn tampered_or_foreign_lists_are_refused() {
        let me = IdentityKeyPair::generate();
        let good = DeviceList::sign(&me, 5, [DeviceId(1), DeviceId(2)])
            .unwrap()
            .encode();
        // Version, a device id, the signature, and the identity itself.
        for at in [32, 41, good.len() - 1, 0] {
            let mut bad = good.clone();
            bad[at] ^= 1;
            assert!(DeviceList::decode(&bad).is_err(), "byte {at}");
        }
        let other = IdentityKeyPair::generate();
        let mut foreign = good.clone();
        foreign[..32].copy_from_slice(other.peer_id().as_bytes());
        assert_eq!(DeviceList::decode(&foreign), Err(CryptoError::Signature));
        DeviceList::decode(&good[..good.len() - 1]).unwrap_err();
        DeviceList::decode(&[good.as_slice(), &[0]].concat()).unwrap_err();
    }

    /// Lists that a careless or hostile signer could produce never decode:
    /// unsorted, duplicated, zero, empty or too many ids.
    #[test]
    fn non_canonical_lists_are_refused_even_when_signed() {
        let me = IdentityKeyPair::generate();
        let raw = |ids: &[u32]| {
            let mut body = me.peer_id().to_vec();
            body.extend_from_slice(&1u64.to_le_bytes());
            body.push(u8::try_from(ids.len()).unwrap());
            for id in ids {
                body.extend_from_slice(&id.to_le_bytes());
            }
            let sig = me.sign(&[DEVICES_SIGNATURE_CONTEXT, &body].concat());
            [body, sig.to_bytes().to_vec()].concat()
        };
        DeviceList::decode(&raw(&[1, 2])).unwrap();
        for ids in [&[2, 1][..], &[1, 1], &[0, 1], &[], &[1, 2, 3, 4, 5, 6, 7]] {
            assert_eq!(
                DeviceList::decode(&raw(ids)),
                Err(CryptoError::Malformed),
                "{ids:?}"
            );
        }
    }
}
