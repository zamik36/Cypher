use std::collections::{BTreeMap, VecDeque};

use bytes::Bytes;
use cypher_crypto::DeviceList;
use cypher_types::{Addr, MsgId, PeerId};
use cypher_wire::ClientMsg;

use super::devices::{DevicesFor, Linked};
use crate::api::CommandId;
use crate::link::LinkOffer;
use crate::share::ShareLink;

/// Anonymous requests that wait for the relay at most; more fail at once.
pub(super) const MAX_DEFERRED: usize = 64;
pub(super) const REQUEST_TIMEOUT_MS: u64 = 15_000;

#[derive(Debug)]
pub(super) enum Pending {
    CreateLink {
        id: CommandId,
    },
    Resolve {
        id: CommandId,
        link: ShareLink,
    },
    /// Keys of one device of the host of an invite we are joining by.
    FetchKeys {
        link: String,
        addr: Addr,
    },
    /// Keys for a session with one device of a contact: one it has none
    /// with yet, or one whose messages stopped decrypting.
    SessionKeys {
        addr: Addr,
    },
    FetchDevices {
        peer: PeerId,
        purpose: DevicesFor,
    },
    PublishDevices {
        list: DeviceList,
        /// A device this one just linked, listed by this publication.
        linked: Option<Linked>,
    },
    /// The hand-over to a new device, for `LinkDevice` `id`.
    LinkSend {
        id: CommandId,
        offer: LinkOffer,
    },
    Publish {
        batch: bool,
    },
    Send {
        msg_id: MsgId,
        addr: Addr,
        body: Bytes,
    },
    /// The same message sent without our name, through the relay.
    SendSealed {
        msg_id: MsgId,
        addr: Addr,
        body: Bytes,
    },
    InboxPut {
        msg_id: MsgId,
        addr: Addr,
    },
    /// A read of the inbox of `epoch`; its last one, which closes it, if
    /// `close`.
    InboxFetch {
        epoch: u32,
        close: bool,
    },
    InboxAck {
        epoch: u32,
        close: bool,
    },
    Bootstrap,
    PushKey,
    /// The gateway took this device's access tokens.
    Access,
    PushRegister {
        epoch: u32,
    },
    PushUnregister,
}

#[derive(Default)]
pub(super) struct Requests {
    pub(super) pending: BTreeMap<u32, (Pending, u64)>,
    /// Anonymous requests made before it was known how they can go: sent
    /// once the relay is up, or its wait is over.
    pub(super) deferred: VecDeque<(ClientMsg, Pending)>,
    next_req: u32,
}

impl Requests {
    pub(super) fn alloc_req(&mut self) -> u32 {
        self.next_req = self.next_req.wrapping_add(1).max(1);
        self.next_req
    }
}
