//! What the core's parts do as seen from outside, pinned so that splitting
//! `Core` into smaller machines cannot change it unnoticed: requests and
//! their failures, messages on their way, invites, transfers, and
//! properties over random runs.
//!
//! Timeouts, waits and limits are written out here rather than taken from
//! the core on purpose: changing one changes behaviour, and these tests
//! should say so.

mod contacts;
mod outbox;
mod properties;
mod requests;
pub(crate) mod rows;
mod transfers;

use bytes::Bytes;
use cypher_core::Event;
use cypher_core::relay::RelayBody;
use cypher_crypto::IdentitySeed;
use cypher_types::{Addr, DeviceId, PeerId};
use cypher_wire::ClientMsg;

use crate::harness::World;

/// A world with two contacts whose first tick is over: the first one
/// tells every contact the device's inboxes again.
fn settled() -> World {
    let mut w = crate::paired();
    w.advance(1_000);
    w
}

/// The device client `i` is.
fn addr(w: &World, i: usize) -> Addr {
    Addr::new(w.peer(i), w.clients[i].device)
}

/// An item sealed to client `i`'s identity, as an inbox holds it or a
/// sealed delivery carries it: who sent it, and the body.
fn open_item(w: &World, i: usize, item: &[u8]) -> Option<(Addr, Bytes)> {
    let secret = IdentitySeed(w.clients[i].seed).derive_identity().dh_secret;
    let plain = cypher_crypto::sealed::open(&secret, item).ok()?;
    let (from, rest) = plain.split_first_chunk::<32>()?;
    let (device, body) = rest.split_first_chunk::<4>()?;
    let from = Addr::new(PeerId(*from), DeviceId(u32::from_le_bytes(*device)));
    Some((from, Bytes::copy_from_slice(body)))
}

/// The items client `i` has waiting in its inbox of `epoch` that `from`
/// left there.
fn inbox_items_from(w: &World, i: usize, epoch: u32, from: Addr) -> usize {
    w.inbox_contents(&w.inbox_of(i, epoch))
        .iter()
        .filter(|item| open_item(w, i, item).is_some_and(|(sender, _)| sender == from))
        .count()
}

/// A frame body that carries an end-to-end message.
fn is_message(body: &Bytes) -> bool {
    matches!(RelayBody::decode(body), Ok(RelayBody::Message { .. }))
}

/// A frame body that carries a file chunk.
fn is_chunk(body: &Bytes) -> bool {
    matches!(RelayBody::decode(body), Ok(RelayBody::Chunk { .. }))
}

/// A frame body that acknowledges file chunks.
fn is_ack(body: &Bytes) -> bool {
    matches!(RelayBody::decode(body), Ok(RelayBody::Ack { .. }))
}

/// An end-to-end message sent over the session to `to`.
fn message_to(msg: &ClientMsg, to: Addr) -> bool {
    matches!(msg, ClientMsg::Send { to: p, device, body, .. }
        if Addr::new(*p, *device) == to && is_message(body))
}

/// The events client `i` was told from the `from`-th on.
fn events_since(w: &World, i: usize, from: usize) -> &[Event] {
    w.clients[i].events.get(from..).unwrap_or_default()
}
