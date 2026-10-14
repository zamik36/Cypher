//! The servers this device has met and the signing key each showed first.
//!
//! A server signs its onion keys with a long-term key. The first time this
//! device connects to a server it remembers that key; from then on it takes
//! onion keys only that key signed, and a server showing another key is not
//! used anonymously until the user says to trust the new one. A server that
//! wanted to recognise one user's anonymous requests would have to have
//! handed them their own key from their very first connection.

use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};

use super::Core;
use crate::api::Event;
use crate::store::{META_SERVERS, Table};

/// Servers remembered at most: the oldest is forgotten beyond.
const MAX_SERVERS: usize = 32;

/// The signing key remembered per server address, kept in `meta`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct ServerKeys {
    pins: Vec<(String, [u8; 32])>,
}

impl crate::Record for ServerKeys {
    const VERSION: u8 = 1;
}

impl ServerKeys {
    fn get(&self, server: &str) -> Option<&[u8; 32]> {
        self.pins.iter().find(|(s, _)| s == server).map(|(_, k)| k)
    }

    fn pin(&mut self, server: &str, key: [u8; 32]) {
        self.pins.retain(|(s, _)| s != server);
        self.pins.push((server.to_owned(), key));
        if self.pins.len() > MAX_SERVERS {
            self.pins.remove(0);
        }
    }
}

impl<R: CryptoRngCore> Core<R> {
    /// The signing key the server showed, if it is the one remembered for
    /// it (or the first it ever showed). Another key is reported and not
    /// used until the user trusts it.
    pub(super) fn checked_server_key(&mut self, shown: [u8; 32]) -> Option<[u8; 32]> {
        match self.server_keys.get(&self.server) {
            Some(known) if *known == shown => Some(shown),
            Some(_) => {
                self.offered_key = Some(shown);
                let server = self.server.clone();
                self.emit(Event::ServerKeyChanged { server });
                None
            }
            None => {
                self.remember_server_key(shown);
                Some(shown)
            }
        }
    }

    /// The user trusts the key the server showed instead of the one
    /// remembered: it is remembered now, and the onion is used again.
    pub(super) fn trust_server_key(&mut self) {
        let Some(key) = self.offered_key.take() else {
            return;
        };
        self.remember_server_key(key);
        self.anon.on_bootstrap(Some(key), self.now);
        self.ask_key_set();
    }

    fn remember_server_key(&mut self, key: [u8; 32]) {
        let server = self.server.clone();
        self.server_keys.pin(&server, key);
        let op = self.vault.put(
            Table::Meta,
            META_SERVERS.to_vec(),
            &self.server_keys,
            &mut self.rng,
        );
        self.persist(op);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_remembered_per_server_and_the_oldest_forgotten() {
        let mut keys = ServerKeys::default();
        keys.pin("a:1", [1; 32]);
        keys.pin("b:1", [2; 32]);
        keys.pin("a:1", [3; 32]);
        assert_eq!(keys.get("a:1"), Some(&[3; 32]));
        assert_eq!(keys.get("b:1"), Some(&[2; 32]));
        assert_eq!(keys.get("c:1"), None);
        for n in 0..MAX_SERVERS {
            keys.pin(&format!("s{n}"), [9; 32]);
        }
        assert_eq!(keys.pins.len(), MAX_SERVERS);
        assert_eq!(keys.get("b:1"), None, "the oldest went first");
    }
}
