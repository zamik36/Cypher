use cypher_types::{Addr, LinkId, PeerId};

fn key(prefix: &[u8], id: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(prefix.len() + id.len());
    k.extend_from_slice(prefix);
    k.extend_from_slice(id);
    k
}

/// Key of something one device of a peer owns: `prefix ‖ peer ‖ device`.
fn device_key(prefix: &[u8], addr: Addr) -> Vec<u8> {
    let mut k = key(prefix, addr.peer.as_bytes());
    k.extend_from_slice(&addr.device.0.to_le_bytes());
    k
}

pub(super) fn bundle(addr: Addr) -> Vec<u8> {
    device_key(b"k:", addr)
}

pub(super) fn opks(addr: Addr) -> Vec<u8> {
    device_key(b"o:", addr)
}

pub(super) fn devices(peer: &PeerId) -> Vec<u8> {
    key(b"d:", peer.as_bytes())
}

pub(super) fn device_version(peer: &PeerId) -> Vec<u8> {
    key(b"dv:", peer.as_bytes())
}

pub(super) fn unlisted(peer: &PeerId) -> Vec<u8> {
    key(b"u:", peer.as_bytes())
}

pub(super) fn link(link: &LinkId) -> Vec<u8> {
    key(b"l:", link.as_str().as_bytes())
}

pub(super) fn onion(name: &[u8]) -> Vec<u8> {
    key(b"x:", name)
}

pub(super) fn replay(replay_id: &[u8; 32]) -> Vec<u8> {
    key(b"r:", replay_id)
}

pub(super) fn rate_limit(bucket: &[u8]) -> Vec<u8> {
    key(b"q:", bucket)
}

pub(super) fn push(inbox: &[u8; 32]) -> Vec<u8> {
    key(b"p:", inbox)
}

pub(super) fn inbox_items(inbox: &[u8; 32]) -> Vec<u8> {
    key(b"i:", inbox)
}

pub(super) fn inbox_bytes(inbox: &[u8; 32]) -> Vec<u8> {
    key(b"b:", inbox)
}

pub(super) fn inbox_opening(inbox: &[u8; 32]) -> Vec<u8> {
    key(b"a:", inbox)
}

pub(super) fn inbox_claim(inbox: &[u8; 32]) -> Vec<u8> {
    key(b"c:", inbox)
}

pub(super) fn budget_counter(scope: &[u8], day: u64) -> Vec<u8> {
    [scope, b"ib:", &day.to_le_bytes()].concat()
}

#[cfg(test)]
pub(super) fn custom(prefix: &[u8], id: &[u8]) -> Vec<u8> {
    key(prefix, id)
}
