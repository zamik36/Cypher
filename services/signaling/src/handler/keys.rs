use cypher_crypto::{DeviceList, PrekeyBundle};
use cypher_types::{Addr, DeviceId, PeerId};
use cypher_wire::{ErrorCode, ServerMsg};

use super::{
    DEVICE_FETCHES_PER_HOUR, DEVICE_PUBLISHES_PER_HOUR, HOUR_SECS, Handler, KEY_FETCHES_PER_HOUR,
    KEY_PUBLISHES_PER_HOUR, OPKS_PER_TARGET_PER_HOUR, UNLISTED_DEVICES_WITH_KEYS, error,
};

impl Handler {
    pub(super) async fn publish_keys(
        &self,
        peer: &PeerId,
        base: &[u8],
        opks: &[(u32, [u8; 32])],
        replace: bool,
    ) -> redis::RedisResult<ServerMsg> {
        let mut full = base.to_vec();
        full.push(0);
        // Any device of the identity may publish; the bundle names which.
        let device = PrekeyBundle::decode(&full)
            .ok()
            .filter(|b| b.identity == *peer && b.verify().is_ok())
            .map(|b| b.device);
        let Some(device) = device else {
            return Ok(error(ErrorCode::BadRequest));
        };
        if !self.admit(b"pk:", peer, KEY_PUBLISHES_PER_HOUR).await? {
            return Ok(error(ErrorCode::RateLimited));
        }
        let addr = Addr::new(*peer, device);
        self.make_room_for_keys(addr).await?;
        let opks_left = self.store.publish_keys(addr, base, opks, replace).await?;
        Ok(ServerMsg::KeysAck { opks_left })
    }

    /// Lets the device `addr` hold keys. One its identity lists (the first
    /// device, before there is a list) always may. One it does not is
    /// about to be listed, since a device being linked or recovered
    /// publishes its keys before its list names it; only a few such
    /// devices keep keys, so the one that published longest ago loses them.
    async fn make_room_for_keys(&self, addr: Addr) -> redis::RedisResult<()> {
        let list = self.store.fetch_devices(&addr.peer).await?;
        let listed = match list.as_deref().map(DeviceList::decode) {
            Some(Ok(list)) => list.contains(addr.device),
            Some(Err(_)) | None => addr.device == DeviceId::FIRST,
        };
        if listed {
            return Ok(());
        }
        let evicted = self
            .store
            .note_unlisted(addr, UNLISTED_DEVICES_WITH_KEYS)
            .await?;
        match evicted {
            Some(device) => {
                self.store
                    .forget_device_keys(Addr::new(addr.peer, device))
                    .await
            }
            None => Ok(()),
        }
    }

    /// A device's keys. One identity asking is limited per hour; through the
    /// onion only the one-time prekeys are rationed, per target, since a
    /// hard limit per target would let anyone stop a victim's new contacts.
    pub(super) async fn fetch_keys(
        &self,
        requester: Option<&PeerId>,
        target: Addr,
    ) -> redis::RedisResult<ServerMsg> {
        if let Some(requester) = requester
            && !self.admit(b"fk:", requester, KEY_FETCHES_PER_HOUR).await?
        {
            return Ok(error(ErrorCode::RateLimited));
        }
        // One-time prekeys are spent per device, so they are rationed per
        // device too.
        let device = target.device.0.to_le_bytes();
        let bucket = [b"ok:", target.peer.as_bytes().as_slice(), &device].concat();
        let with_opk = self
            .store
            .admit(&bucket, OPKS_PER_TARGET_PER_HOUR, HOUR_SECS)
            .await?;
        Ok(match self.store.fetch_keys(target, with_opk).await? {
            Some((base, opk)) => ServerMsg::Keys { base, opk },
            None => error(ErrorCode::NotFound),
        })
    }

    /// Replaces `peer`'s device list with a newer one it signed, and forgets
    /// the keys of the devices it dropped.
    pub(super) async fn publish_devices(
        &self,
        peer: &PeerId,
        list: &[u8],
    ) -> redis::RedisResult<ServerMsg> {
        let Ok(new) = DeviceList::decode(list) else {
            return Ok(error(ErrorCode::BadRequest));
        };
        if new.identity() != *peer {
            return Ok(error(ErrorCode::BadRequest));
        }
        if !self.admit(b"pd:", peer, DEVICE_PUBLISHES_PER_HOUR).await? {
            return Ok(error(ErrorCode::RateLimited));
        }
        let old = self.store.fetch_devices(peer).await?;
        if !self
            .store
            .publish_devices(peer, new.version(), list)
            .await?
        {
            return Ok(error(ErrorCode::Conflict));
        }
        // Best effort: keys expire on their own anyway. Devices that held
        // keys before a list named them count as listed until now.
        let old = old.and_then(|o| DeviceList::decode(&o).ok());
        let unlisted = self.store.take_unlisted(peer).await?;
        let held = old.iter().flat_map(DeviceList::devices).chain(&unlisted);
        for &device in held {
            if !new.contains(device) {
                self.store
                    .forget_device_keys(Addr::new(*peer, device))
                    .await?;
            }
        }
        Ok(ServerMsg::Done)
    }

    /// An identity's device list, limited per identity asking; through the
    /// onion only the relay's limits apply.
    pub(super) async fn fetch_devices(
        &self,
        requester: Option<&PeerId>,
        target: &PeerId,
    ) -> redis::RedisResult<ServerMsg> {
        if let Some(requester) = requester
            && !self
                .admit(b"fd:", requester, DEVICE_FETCHES_PER_HOUR)
                .await?
        {
            return Ok(error(ErrorCode::RateLimited));
        }
        Ok(match self.store.fetch_devices(target).await? {
            Some(list) => ServerMsg::Devices { list },
            None => error(ErrorCode::NotFound),
        })
    }
}
