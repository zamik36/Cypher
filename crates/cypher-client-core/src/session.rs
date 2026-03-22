use cypher_common::PeerId;
use cypher_crypto::identity::IdentityKeyPair;

/// An ephemeral client session backed by a freshly generated identity keypair.
pub struct ClientSession {
    pub identity: IdentityKeyPair,
    pub peer_id: PeerId,
}

impl ClientSession {
    /// Generate a new ephemeral identity and derive the peer-id from it.
    pub fn new() -> Self {
        let identity = IdentityKeyPair::generate();
        let peer_id = identity.peer_id();
        Self { identity, peer_id }
    }

    pub fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }
}

impl Default for ClientSession {
    fn default() -> Self {
        Self::new()
    }
}
