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

#[cfg(test)]
mod tests {
    use cypher_crypto::IdentitySeed;
    use cypher_types::{DeviceId, LinkId};
    use cypher_wire::{Frame, ServerMsg};
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    use super::super::{Conn, Core, Snapshot};
    use super::*;
    use crate::api::{Command, Effect, Event, Input};

    fn ready_core() -> Core<ChaCha20Rng> {
        let (mut core, _) = Core::restore(
            &IdentitySeed([1; 32]),
            DeviceId::FIRST,
            &Snapshot::default(),
            0,
            ChaCha20Rng::seed_from_u64(7),
        )
        .unwrap();
        core.connection.conn = Conn::Ready;
        core
    }

    fn link(n: u64) -> LinkId {
        LinkId::parse(&cypher_types::base32(&u128::from(n).to_be_bytes())).unwrap()
    }

    #[test]
    fn request_ids_wrap_and_keep_response_correlation() {
        let mut core = ready_core();
        core.requests.next_req = u32::MAX - 1;
        let mut sent = Vec::new();
        for command in [10, 11, 12] {
            let effects = core.handle(
                Input::Command(Command::CreateLink {
                    id: CommandId(command),
                }),
                0,
            );
            let [Effect::Transmit(raw)] = effects.as_slice() else {
                panic!("one CreateLink transmission: {effects:?}");
            };
            let frame = Frame::<ClientMsg>::decode(raw.clone()).unwrap();
            assert!(matches!(frame.msg, ClientMsg::CreateLink));
            sent.push((frame.req_id, command));
        }
        assert_eq!(sent, [(u32::MAX, 10), (1, 11), (2, 12)]);

        for (req_id, command) in [(1, 11), (2, 12), (u32::MAX, 10)] {
            let effects = core.handle(
                Input::Frame(
                    Frame::new(
                        req_id,
                        ServerMsg::LinkCreated {
                            link: link(command),
                        },
                    )
                    .encode(),
                ),
                0,
            );
            let answered: Vec<_> = effects
                .iter()
                .filter_map(|effect| match effect {
                    Effect::Emit(Event::LinkCreated { id, .. }) => Some(*id),
                    _ => None,
                })
                .collect();
            assert_eq!(answered, [CommandId(command)]);
        }
        assert!(core.requests.pending.is_empty());
    }
}
