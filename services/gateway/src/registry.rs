use cypher_types::Addr;
use dashmap::DashMap;
use tokio_util::sync::CancellationToken;

use crate::outbox::Outbox;

#[derive(Clone)]
pub struct ConnHandle {
    pub conn_id: u64,
    pub outbox: Outbox,
    pub kick: CancellationToken,
}

/// Authenticated sessions on this node, one per device of a peer, for
/// zero-hop local delivery.
#[derive(Default)]
pub struct Registry {
    devices: DashMap<Addr, ConnHandle>,
}

impl Registry {
    /// Registers `handle`, returning the session of the same device it
    /// replaced.
    pub fn insert(&self, addr: Addr, handle: ConnHandle) -> Option<ConnHandle> {
        self.devices.insert(addr, handle)
    }

    /// Clones the handle out so no map guard outlives this call.
    pub fn get(&self, addr: &Addr) -> Option<ConnHandle> {
        self.devices.get(addr).map(|h| h.value().clone())
    }

    /// Removes the entry only if it still belongs to `conn_id`.
    pub fn remove(&self, addr: &Addr, conn_id: u64) {
        self.devices.remove_if(addr, |_, h| h.conn_id == conn_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbox::outbox;
    use cypher_types::{DeviceId, PeerId};

    fn handle(id: u64) -> ConnHandle {
        ConnHandle {
            conn_id: id,
            outbox: outbox().0,
            kick: CancellationToken::new(),
        }
    }

    #[test]
    fn stale_remove_does_not_evict_newer_session() {
        let r = Registry::default();
        let p = Addr::new(PeerId([1; 32]), DeviceId::FIRST);
        assert!(r.insert(p, handle(1)).is_none());
        assert_eq!(r.insert(p, handle(2)).unwrap().conn_id, 1);
        r.remove(&p, 1);
        assert_eq!(r.get(&p).unwrap().conn_id, 2);
        r.remove(&p, 2);
        assert!(r.get(&p).is_none());
    }

    #[test]
    fn devices_of_one_peer_are_separate_sessions() {
        let r = Registry::default();
        let first = Addr::new(PeerId([1; 32]), DeviceId::FIRST);
        let second = Addr::new(PeerId([1; 32]), DeviceId(2));
        assert!(r.insert(first, handle(1)).is_none());
        assert!(r.insert(second, handle(2)).is_none());
        assert_eq!(r.get(&first).unwrap().conn_id, 1);
        assert_eq!(r.get(&second).unwrap().conn_id, 2);
    }
}
