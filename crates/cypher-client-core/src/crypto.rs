use dashmap::DashMap;
use cypher_common::{Error, Result};
use cypher_crypto::identity::{IdentityKeyPair, KeyBundle, SignedPreKey};
use cypher_crypto::ratchet::RatchetState;
use cypher_crypto::x3dh::SharedSecret;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519StaticSecret};

/// Manages session keys for a single local identity.
///
/// Thread-safe: wraps per-peer ratchet states in a [`DashMap`] so that
/// multiple tasks can encrypt/decrypt concurrently for different peers.
pub struct KeyManager {
    identity: IdentityKeyPair,
    /// The signed pre-key published as part of our [`KeyBundle`].
    spk: SignedPreKey,
    /// Per-peer Double-Ratchet state, keyed by peer_id bytes.
    peer_sessions: DashMap<Vec<u8>, RatchetState>,
}

impl KeyManager {
    pub fn new(identity: IdentityKeyPair) -> Self {
        let spk = SignedPreKey::generate();
        Self {
            identity,
            spk,
            peer_sessions: DashMap::new(),
        }
    }

    pub fn identity(&self) -> &IdentityKeyPair {
        &self.identity
    }

    /// Build the [`KeyBundle`] that should be uploaded to the server so that
    /// peers can initiate an X3DH handshake with us.
    pub fn key_bundle(&self) -> KeyBundle {
        KeyBundle::new(&self.identity, &self.spk)
    }

    pub fn spk_secret(&self) -> X25519StaticSecret {
        self.spk.secret.clone()
    }

    /// Initialise a sender-side ratchet session for `peer_id`.
    ///
    /// Called after completing the X3DH handshake as the initiator.
    pub fn init_sender_session(
        &self,
        peer_id: &[u8],
        shared_secret: &SharedSecret,
        peer_ratchet_pub: X25519PublicKey,
    ) {
        let state = RatchetState::init_sender(shared_secret, &peer_ratchet_pub);
        self.peer_sessions.insert(peer_id.to_vec(), state);
    }

    /// Initialise a receiver-side ratchet session for `peer_id`.
    ///
    /// The receiver provides their own SPK secret; the send chain is set up
    /// when the first message from the sender triggers a DH ratchet step.
    pub fn init_receiver_session(
        &self,
        peer_id: &[u8],
        shared_secret: &SharedSecret,
    ) {
        let state = RatchetState::init_receiver(shared_secret, self.spk.secret.clone());
        self.peer_sessions.insert(peer_id.to_vec(), state);
    }

    /// Encrypt `plaintext` for the peer identified by `peer_id`.
    ///
    /// Returns `(ciphertext, ratchet_key_bytes, message_number)`.
    ///
    /// Errors if no session has been established with this peer.
    pub fn encrypt_for_peer(
        &self,
        peer_id: &[u8],
        plaintext: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>, u32)> {
        let mut state = self
            .peer_sessions
            .get_mut(peer_id)
            .ok_or_else(|| Error::Session(format!("no session for peer {:?}", peer_id)))?;

        let (ciphertext, ratchet_key, msg_no) = state.encrypt(plaintext)?;
        Ok((ciphertext, ratchet_key.as_bytes().to_vec(), msg_no))
    }

    /// Decrypt a message received from the peer identified by `peer_id`.
    ///
    /// `ratchet_key` must be the 32-byte X25519 public key included in the
    /// message header.
    ///
    /// Errors if no session has been established with this peer or if
    /// decryption fails.
    pub fn decrypt_from_peer(
        &self,
        peer_id: &[u8],
        ciphertext: &[u8],
        ratchet_key: &[u8],
        msg_no: u32,
    ) -> Result<Vec<u8>> {
        let rk_bytes: [u8; 32] = ratchet_key
            .try_into()
            .map_err(|_| Error::Crypto("ratchet_key must be 32 bytes".into()))?;
        let rk = X25519PublicKey::from(rk_bytes);

        let mut state = self
            .peer_sessions
            .get_mut(peer_id)
            .ok_or_else(|| Error::Session(format!("no session for peer {:?}", peer_id)))?;

        state.decrypt(ciphertext, &rk, msg_no)
    }
}
