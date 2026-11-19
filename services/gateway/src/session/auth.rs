use std::io;

use bytes::Bytes;
use cypher_types::{Addr, session_auth_message};
use cypher_wire::{ClientMsg, ErrorCode, Frame, PROTOCOL_VERSION, ServerMsg};
use ed25519_dalek::{Signature, VerifyingKey};
use futures::{Stream, StreamExt};
use rand::RngCore;

use crate::bus::Bus;

use super::{Session, error, frame};

impl<B: Bus> Session<B> {
    /// `Hello` → `Challenge` → `Auth` proof-of-possession of the identity
    /// key, for the device the client names.
    pub(super) async fn authenticate<S>(&self, stream: &mut S) -> Option<Addr>
    where
        S: Stream<Item = io::Result<Bytes>> + Unpin + Send,
    {
        let hello = Frame::<ClientMsg>::decode(stream.next().await?.ok()?).ok()?;
        let ClientMsg::Hello {
            version,
            peer,
            device,
        } = hello.msg
        else {
            return None;
        };
        if version != PROTOCOL_VERSION {
            self.out
                .try_push(frame(hello.req_id, error(ErrorCode::UnsupportedVersion)));
            return None;
        }
        let mut nonce = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        self.out
            .try_push(frame(hello.req_id, ServerMsg::Challenge { nonce }));

        let auth = Frame::<ClientMsg>::decode(stream.next().await?.ok()?).ok()?;
        let ClientMsg::Auth { signature } = auth.msg else {
            return None;
        };
        let signed = session_auth_message(&nonce, device);
        // Strict: a small-order key would otherwise "sign" any challenge.
        let verified = VerifyingKey::from_bytes(peer.as_bytes())
            .and_then(|key| key.verify_strict(&signed, &Signature::from_bytes(&signature)));
        if verified.is_err() {
            self.gw.metrics.auth_failures.inc();
            self.out
                .try_push(frame(auth.req_id, error(ErrorCode::Unauthorized)));
            return None;
        }
        Some(Addr::new(peer, device))
    }
}
