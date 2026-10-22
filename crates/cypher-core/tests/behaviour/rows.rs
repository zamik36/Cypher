//! Rows a client stores, read back in the form the core writes them now,
//! to check what is kept, not only what is shown.

use cypher_core::{MessageStatus, Record, StoreOp, Table, Vault, session_key};
use cypher_crypto::IdentitySeed;
use cypher_types::{Addr, DeviceId, MsgId, PeerId};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

use crate::harness::World;

/// An inbox announcement: epoch, this epoch's and the next inbox, and
/// their access tokens.
pub(crate) type Ad = (u32, [[u8; 32]; 2], [[u8; 32]; 2]);

/// A session with one device.
#[derive(Serialize, Deserialize)]
pub(crate) struct Session {
    pub ratchet: Vec<u8>,
    /// Our init header, while the device has not answered; empty after.
    pub pending_init: Vec<u8>,
    /// The ephemeral keys of the inits this side accepted from the device.
    pub accepted_ephemerals: Vec<[u8; 32]>,
    pub inbox: Option<Ad>,
    pub hello_sent: bool,
    /// The line of sessions with the device; rows beside it carry it.
    pub lineage: u64,
    /// Skipped keys whose rows are deleted right after this one is stored.
    pub spent: Vec<u64>,
}

impl Record for Session {
    const VERSION: u8 = 3;
}

/// One device a message goes to.
#[derive(Serialize, Deserialize)]
pub(crate) struct Target {
    pub device: DeviceId,
    /// Taken by the server, waiting for the device's receipt.
    pub awaiting: bool,
}

/// A message on its way, or the record of its receipts.
#[derive(Serialize, Deserialize)]
pub(crate) struct Outbox {
    pub msg_id: MsgId,
    pub peer: PeerId,
    /// The padded message, empty once nothing is left to send.
    pub envelope: Vec<u8>,
    /// A message the user sees, as opposed to control traffic.
    pub tracked: bool,
    pub targets: Vec<Target>,
    pub reported: MessageStatus,
}

impl Record for Outbox {
    const VERSION: u8 = 3;
}

/// The invites this user made and when.
#[derive(Serialize, Deserialize)]
struct Links {
    links: Vec<(String, u64)>,
}

impl Record for Links {
    const VERSION: u8 = 1;
}

/// The prekeys as version 2 stores them.
#[derive(Serialize, Deserialize)]
struct Prekeys {
    spk: (u32, [u8; 32]),
    spk_created_ms: u64,
    prev_spk: Option<(u32, [u8; 32])>,
    opks: Vec<(u32, [u8; 32])>,
    next_opk_id: u32,
    spk_inits: Vec<[u8; 32]>,
    prev_spk_inits: Vec<[u8; 32]>,
}

impl Record for Prekeys {
    const VERSION: u8 = 2;
}

/// The prekeys as version 1 stored them, without the inits accepted.
#[derive(Serialize, Deserialize)]
struct PrekeysV1 {
    spk: (u32, [u8; 32]),
    spk_created_ms: u64,
    prev_spk: Option<(u32, [u8; 32])>,
    opks: Vec<(u32, [u8; 32])>,
    next_opk_id: u32,
}

impl Record for PrekeysV1 {
    const VERSION: u8 = 1;
}

/// Rewrites client `i`'s prekeys as version 1 stored them, as if the app
/// had just been updated.
pub(crate) fn store_prekeys_as_v1(w: &mut World, i: usize) {
    let key = b"prekeys".to_vec();
    let row = (Table::Meta as u8, key.clone());
    let vault = vault(w, i);
    let now: Prekeys = vault
        .open(Table::Meta, &key, &w.clients[i].kv[&row])
        .unwrap();
    let old = PrekeysV1 {
        spk: now.spk,
        spk_created_ms: now.spk_created_ms,
        prev_spk: now.prev_spk,
        opks: now.opks,
        next_opk_id: now.next_opk_id,
    };
    let StoreOp::Put { value, .. } = vault.put(Table::Meta, key, &old, &mut OsRng) else {
        unreachable!("a put")
    };
    w.clients[i].kv.insert(row, value);
}

fn vault(w: &World, i: usize) -> Vault {
    Vault::new(IdentitySeed(w.clients[i].seed).derive_storage_key())
}

/// Every outbox row of client `i`.
pub(crate) fn outbox(w: &World, i: usize) -> Vec<Outbox> {
    let vault = vault(w, i);
    w.clients[i]
        .kv
        .iter()
        .filter(|((table, _), _)| *table == Table::Outbox as u8)
        .map(|((_, key), value)| vault.open(Table::Outbox, key, value).unwrap())
        .collect()
}

/// The outbox row of message `id` at client `i`.
pub(crate) fn outbox_row(w: &World, i: usize, id: MsgId) -> Option<Outbox> {
    outbox(w, i).into_iter().find(|row| row.msg_id == id)
}

/// The invite codes client `i` keeps.
pub(crate) fn invites(w: &World, i: usize) -> Vec<String> {
    let key = b"links".to_vec();
    let Some(value) = w.clients[i].kv.get(&(Table::Meta as u8, key.clone())) else {
        return Vec::new();
    };
    let links: Links = vault(w, i).open(Table::Meta, &key, value).unwrap();
    links.links.into_iter().map(|(code, _)| code).collect()
}

/// Client `i`'s session with `addr`.
pub(crate) fn session(w: &World, i: usize, addr: Addr) -> Option<Session> {
    let key = session_key(addr);
    let value = w.clients[i].kv.get(&(Table::Sessions as u8, key.clone()))?;
    Some(vault(w, i).open(Table::Sessions, &key, value).unwrap())
}
