use std::collections::HashMap;

use cypher_common::{Error, Result};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use sha2::Sha256;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519StaticSecret};
use zeroize::Zeroize;

use crate::aead::{aead_decrypt, aead_encrypt};
use crate::x3dh::SharedSecret;

/// Perform a KDF chain step: derive a new chain key and a message key from the current chain key.
///
/// Returns `(new_chain_key, message_key)`.
pub fn kdf_chain(chain_key: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<Sha256>::new(Some(chain_key), &[0x01]);
    let mut new_chain_key = [0u8; 32];
    hk.expand(b"chain-key", &mut new_chain_key)
        .expect("32 bytes is valid for HKDF-SHA256");

    let hk2 = Hkdf::<Sha256>::new(Some(chain_key), &[0x02]);
    let mut message_key = [0u8; 32];
    hk2.expand(b"message-key", &mut message_key)
        .expect("32 bytes is valid for HKDF-SHA256");

    (new_chain_key, message_key)
}

/// Perform a DH ratchet step: derive a new root key and new chain key from a root key and DH output.
///
/// Returns `(new_root_key, new_chain_key)`.
pub fn dh_ratchet(root_key: &[u8; 32], dh_output: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<Sha256>::new(Some(root_key), dh_output);
    let mut new_root_key = [0u8; 32];
    hk.expand(b"dh-ratchet-root", &mut new_root_key)
        .expect("32 bytes is valid for HKDF-SHA256");

    let mut new_chain_key = [0u8; 32];
    hk.expand(b"dh-ratchet-chain", &mut new_chain_key)
        .expect("32 bytes is valid for HKDF-SHA256");

    (new_root_key, new_chain_key)
}

/// Key for looking up skipped message keys: (ratchet public key, message number).
type SkippedKey = ([u8; 32], u32);

/// Maximum number of skipped message keys to store per ratchet public key.
const MAX_SKIP: u32 = 256;

/// The Double Ratchet state for a single session.
pub struct RatchetState {
    /// Current root key.
    pub root_key: [u8; 32],
    /// Current sending chain key (None before first DH ratchet on sender side).
    pub send_chain_key: Option<[u8; 32]>,
    /// Current receiving chain key (None before receiving first message with new ratchet key).
    pub recv_chain_key: Option<[u8; 32]>,
    /// Our current DH ratchet secret key.
    pub send_ratchet_secret: X25519StaticSecret,
    /// Our current DH ratchet public key.
    pub send_ratchet_key: X25519PublicKey,
    /// The peer's current DH ratchet public key (None if not yet received).
    pub recv_ratchet_key: Option<X25519PublicKey>,
    /// Number of messages sent in the current sending chain.
    pub send_count: u32,
    /// Number of messages received in the current receiving chain.
    pub recv_count: u32,
    /// Previous sending chain length (for header).
    pub prev_send_count: u32,
    /// Skipped message keys for out-of-order decryption.
    pub skipped_keys: HashMap<SkippedKey, [u8; 32]>,
}

impl RatchetState {
    /// Initialize the ratchet state for the sender (initiator, Alice).
    ///
    /// The sender performs an initial DH ratchet step using the peer's ratchet public key.
    pub fn init_sender(
        shared_secret: &SharedSecret,
        peer_ratchet_pubkey: &X25519PublicKey,
    ) -> Self {
        let our_ratchet_secret = X25519StaticSecret::random_from_rng(OsRng);
        let our_ratchet_public = X25519PublicKey::from(&our_ratchet_secret);

        let dh_output = our_ratchet_secret.diffie_hellman(peer_ratchet_pubkey);
        let (root_key, send_chain_key) = dh_ratchet(shared_secret.as_bytes(), dh_output.as_bytes());

        Self {
            root_key,
            send_chain_key: Some(send_chain_key),
            recv_chain_key: None,
            send_ratchet_secret: our_ratchet_secret,
            send_ratchet_key: our_ratchet_public,
            recv_ratchet_key: Some(*peer_ratchet_pubkey),
            send_count: 0,
            recv_count: 0,
            prev_send_count: 0,
            skipped_keys: HashMap::new(),
        }
    }

    /// Initialize the ratchet state for the receiver (responder, Bob).
    ///
    /// The receiver provides their initial ratchet keypair; the send chain is set up
    /// when the first message from the sender triggers a DH ratchet step.
    pub fn init_receiver(
        shared_secret: &SharedSecret,
        our_ratchet_secret: X25519StaticSecret,
    ) -> Self {
        let our_ratchet_public = X25519PublicKey::from(&our_ratchet_secret);
        Self {
            root_key: *shared_secret.as_bytes(),
            send_chain_key: None,
            recv_chain_key: None,
            send_ratchet_secret: our_ratchet_secret,
            send_ratchet_key: our_ratchet_public,
            recv_ratchet_key: None,
            send_count: 0,
            recv_count: 0,
            prev_send_count: 0,
            skipped_keys: HashMap::new(),
        }
    }

    /// Encrypt a plaintext message.
    ///
    /// Returns `(ciphertext, ratchet_public_key, message_number)`.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<(Vec<u8>, X25519PublicKey, u32)> {
        let send_chain_key = self.send_chain_key.ok_or_else(|| {
            Error::Crypto("send_chain_key must be initialized before encrypting".into())
        })?;

        let (new_chain_key, message_key) = kdf_chain(&send_chain_key);
        self.send_chain_key = Some(new_chain_key);

        let msg_no = self.send_count;
        self.send_count += 1;

        // Use message number as nonce material.
        let nonce_material = msg_no.to_be_bytes();
        // Use ratchet public key as AAD.
        let aad = self.send_ratchet_key.as_bytes();

        let ciphertext = aead_encrypt(&message_key, &nonce_material, plaintext, aad)?;

        Ok((ciphertext, self.send_ratchet_key, msg_no))
    }

    /// Decrypt a ciphertext message.
    ///
    /// Handles DH ratchet step when a new ratchet public key is received from the peer.
    /// Supports out-of-order messages via skipped_keys.
    pub fn decrypt(
        &mut self,
        ciphertext: &[u8],
        ratchet_pubkey: &X25519PublicKey,
        msg_no: u32,
    ) -> Result<Vec<u8>> {
        // Check if this is a skipped message key.
        let skip_key = (ratchet_pubkey.to_bytes(), msg_no);
        if let Some(message_key) = self.skipped_keys.remove(&skip_key) {
            let nonce_material = msg_no.to_be_bytes();
            let aad = ratchet_pubkey.as_bytes();
            return aead_decrypt(&message_key, &nonce_material, ciphertext, aad);
        }

        // Check if we need a DH ratchet step (new ratchet key from peer).
        let need_dh_ratchet = match &self.recv_ratchet_key {
            None => true,
            Some(existing) => existing.as_bytes() != ratchet_pubkey.as_bytes(),
        };

        if need_dh_ratchet {
            // Perform the DH ratchet step.
            self.prev_send_count = self.send_count;
            self.send_count = 0;
            self.recv_count = 0;
            self.recv_ratchet_key = Some(*ratchet_pubkey);

            // Receiving chain: DH with their new ratchet key and our current secret.
            let dh_recv = self.send_ratchet_secret.diffie_hellman(ratchet_pubkey);
            let (new_root_key, recv_chain_key) = dh_ratchet(&self.root_key, dh_recv.as_bytes());
            self.root_key = new_root_key;
            self.recv_chain_key = Some(recv_chain_key);

            // Generate new sending ratchet keypair.
            self.send_ratchet_secret = X25519StaticSecret::random_from_rng(OsRng);
            self.send_ratchet_key = X25519PublicKey::from(&self.send_ratchet_secret);

            // Sending chain: DH with their ratchet key and our new secret.
            let dh_send = self.send_ratchet_secret.diffie_hellman(ratchet_pubkey);
            let (new_root_key, send_chain_key) = dh_ratchet(&self.root_key, dh_send.as_bytes());
            self.root_key = new_root_key;
            self.send_chain_key = Some(send_chain_key);

            // Now skip messages in the new recv chain up to msg_no.
            self.skip_messages(msg_no)?;
        } else {
            // Same ratchet key: skip any messages we haven't received yet.
            self.skip_messages(msg_no)?;
        }

        // KDF chain step to get the message key.
        let recv_chain_key = self
            .recv_chain_key
            .ok_or_else(|| Error::Crypto("recv_chain_key not initialized".into()))?;
        let (new_chain_key, message_key) = kdf_chain(&recv_chain_key);
        self.recv_chain_key = Some(new_chain_key);
        self.recv_count += 1;

        let nonce_material = msg_no.to_be_bytes();
        let aad = ratchet_pubkey.as_bytes();
        aead_decrypt(&message_key, &nonce_material, ciphertext, aad)
    }

    /// Skip message keys up to `until` and store them for out-of-order decryption.
    fn skip_messages(&mut self, until: u32) -> Result<()> {
        if let (Some(recv_chain_key), Some(recv_ratchet_key)) =
            (self.recv_chain_key, self.recv_ratchet_key)
        {
            let to_skip = until.saturating_sub(self.recv_count);
            if to_skip > MAX_SKIP {
                return Err(Error::Crypto(format!(
                    "Too many skipped messages: {}",
                    to_skip
                )));
            }

            let mut chain_key = recv_chain_key;
            for i in self.recv_count..until {
                let (new_chain_key, message_key) = kdf_chain(&chain_key);
                let key = (recv_ratchet_key.to_bytes(), i);
                self.skipped_keys.insert(key, message_key);
                chain_key = new_chain_key;
            }
            self.recv_chain_key = Some(chain_key);
            self.recv_count = until;
        }
        Ok(())
    }
}

impl Drop for RatchetState {
    fn drop(&mut self) {
        self.root_key.zeroize();
        if let Some(ref mut key) = self.send_chain_key {
            key.zeroize();
        }
        if let Some(ref mut key) = self.recv_chain_key {
            key.zeroize();
        }
        for key in self.skipped_keys.values_mut() {
            key.zeroize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_receiver_roundtrip() {
        let shared_secret = SharedSecret([99u8; 32]);

        // Bob generates his initial ratchet keypair.
        let bob_ratchet_secret = X25519StaticSecret::random_from_rng(OsRng);
        let bob_ratchet_public = X25519PublicKey::from(&bob_ratchet_secret);

        // Alice initializes as sender.
        let mut alice = RatchetState::init_sender(&shared_secret, &bob_ratchet_public);
        // Bob initializes as receiver.
        let mut bob = RatchetState::init_receiver(&shared_secret, bob_ratchet_secret);

        // Alice sends a message.
        let (ct, rk, mn) = alice.encrypt(b"hello bob").unwrap();
        let pt = bob.decrypt(&ct, &rk, mn).unwrap();
        assert_eq!(pt, b"hello bob");

        // Alice sends another message.
        let (ct2, rk2, mn2) = alice.encrypt(b"second message").unwrap();
        let pt2 = bob.decrypt(&ct2, &rk2, mn2).unwrap();
        assert_eq!(pt2, b"second message");

        // Bob replies to Alice (triggers DH ratchet on Bob's side during encrypt setup).
        let (ct3, rk3, mn3) = bob.encrypt(b"hello alice").unwrap();
        let pt3 = alice.decrypt(&ct3, &rk3, mn3).unwrap();
        assert_eq!(pt3, b"hello alice");

        // Alice replies again.
        let (ct4, rk4, mn4) = alice.encrypt(b"back to you").unwrap();
        let pt4 = bob.decrypt(&ct4, &rk4, mn4).unwrap();
        assert_eq!(pt4, b"back to you");
    }

    #[test]
    fn out_of_order_messages() {
        let shared_secret = SharedSecret([42u8; 32]);

        let bob_ratchet_secret = X25519StaticSecret::random_from_rng(OsRng);
        let bob_ratchet_public = X25519PublicKey::from(&bob_ratchet_secret);

        let mut alice = RatchetState::init_sender(&shared_secret, &bob_ratchet_public);
        let mut bob = RatchetState::init_receiver(&shared_secret, bob_ratchet_secret);

        // Alice sends 3 messages.
        let (ct0, rk0, mn0) = alice.encrypt(b"msg 0").unwrap();
        let (ct1, rk1, mn1) = alice.encrypt(b"msg 1").unwrap();
        let (ct2, rk2, mn2) = alice.encrypt(b"msg 2").unwrap();

        // Bob receives them out of order: 2, 0, 1.
        let pt2 = bob.decrypt(&ct2, &rk2, mn2).unwrap();
        assert_eq!(pt2, b"msg 2");

        let pt0 = bob.decrypt(&ct0, &rk0, mn0).unwrap();
        assert_eq!(pt0, b"msg 0");

        let pt1 = bob.decrypt(&ct1, &rk1, mn1).unwrap();
        assert_eq!(pt1, b"msg 1");
    }

    #[test]
    fn kdf_chain_deterministic() {
        let ck = [1u8; 32];
        let (ck1, mk1) = kdf_chain(&ck);
        let (ck2, mk2) = kdf_chain(&ck);
        assert_eq!(ck1, ck2);
        assert_eq!(mk1, mk2);
    }
}
