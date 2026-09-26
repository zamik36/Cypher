use cypher_types::PeerId;
use dashmap::DashMap;
use tokio_util::sync::CancellationToken;

use crate::outbox::Outbox;

#[derive(Clone)]
pub struct ConnHandle {
    pub conn_id: u64,
    pub outbox: Outbox,
    pub kick: CancellationToken,
}

/// Authenticated sessions on this node, for zero-hop local delivery.
#[derive(Default)]
pub struct Registry {
    peers: DashMap<PeerId, ConnHandle>,
}

impl Registry {
    /// Registers `handle`, returning the session it replaced.
    pub fn insert(&self, peer: PeerId, handle: ConnHandle) -> Option<ConnHandle> {
        self.peers.insert(peer, handle)
    }

    /// Clones the handle out so no map guard outlives this call.
    pub fn get(&self, peer: &PeerId) -> Option<ConnHandle> {
        self.peers.get(peer).map(|h| h.value().clone())
    }

    /// Removes the entry only if it still belongs to `conn_id`.
    pub fn remove(&self, peer: &PeerId, conn_id: u64) {
        self.peers.remove_if(peer, |_, h| h.conn_id == conn_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbox::outbox;

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
        let p = PeerId([1; 32]);
        assert!(r.insert(p, handle(1)).is_none());
        assert_eq!(r.insert(p, handle(2)).unwrap().conn_id, 1);
        r.remove(&p, 1);
        assert_eq!(r.get(&p).unwrap().conn_id, 2);
        r.remove(&p, 2);
        assert!(r.get(&p).is_none());
    }
}
