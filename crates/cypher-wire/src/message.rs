use bytes::{BufMut, Bytes, BytesMut};
use cypher_types::{DeviceId, LinkId, PeerId};

use crate::codec::{Reader, WriteExt, field_len};
use crate::{
    BUNDLE_BASE_LEN, FRAME_HEADER_LEN, MAX_ACCESS_HASHES, MAX_BODY_LEN, MAX_DEVICE_LIST_LEN,
    MAX_INBOX_BATCH, MAX_INBOX_ITEM_LEN, MAX_OPKS_PER_PUBLISH, MAX_PUSH_ENDPOINT_LEN,
    MAX_RELAY_ADDR_LEN, WireError,
};

/// A message plus the request id that correlates it with its reply.
/// Unsolicited server pushes use `req_id == 0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame<M> {
    pub req_id: u32,
    pub msg: M,
}

impl<M> Frame<M> {
    pub fn new(req_id: u32, msg: M) -> Self {
        Self { req_id, msg }
    }
}

/// Client → server messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMsg {
    /// Opens a session as one device of an identity.
    Hello {
        version: u16,
        peer: PeerId,
        device: DeviceId,
    },
    Auth {
        signature: [u8; 64],
    },
    Ping,
    /// Relay an opaque end-to-end body to one device of `to`. With
    /// `want_ack` the server answers with [`ServerMsg::SendAck`].
    Send {
        to: PeerId,
        device: DeviceId,
        want_ack: bool,
        body: Bytes,
    },
    /// Delivers `item` to one device of `to` without naming the sender:
    /// only through the onion relay. `token` is the device's access token
    /// for now; `item` is sealed as an inbox item is, and goes to `inbox`,
    /// if given, when the device is away. Answered with
    /// [`ServerMsg::SendAck`].
    SendSealed {
        to: PeerId,
        device: DeviceId,
        token: [u8; 32],
        inbox: Option<[u8; 32]>,
        item: Bytes,
    },
    /// The hashes ([`crate::access_hash`]) of the tokens this device takes
    /// sealed deliveries with, replacing those told before; at most
    /// [`MAX_ACCESS_HASHES`].
    SetAccess {
        hashes: Vec<[u8; 32]>,
    },
    /// `base` is a [`BUNDLE_BASE_LEN`]-byte signed prekey bundle.
    PublishKeys {
        base: Bytes,
        opks: Vec<(u32, [u8; 32])>,
        replace_opks: bool,
    },
    FetchKeys {
        peer: PeerId,
        device: DeviceId,
    },
    /// Replaces the identity's signed device list with a newer version.
    PublishDevices {
        list: Bytes,
    },
    /// The signed device list of `peer`.
    FetchDevices {
        peer: PeerId,
    },
    CreateLink,
    ResolveLink {
        link: LinkId,
    },
    InboxPut {
        inbox: [u8; 32],
        item: Bytes,
    },
    /// The items waiting in the inbox; with `close`, the last fetch of an
    /// inbox no longer announced, which then stops taking items.
    InboxFetch {
        proof: InboxProof,
        close: bool,
    },
    InboxAck {
        proof: InboxProof,
        claim: [u8; 16],
    },
    Bootstrap,
    /// The server's VAPID public key, for subscribing to Web Push.
    PushKey,
    /// Signal `endpoint` whenever the inbox receives an item. Anonymous
    /// only: tying it to the session would link push and identity.
    PushRegister {
        proof: InboxProof,
        endpoint: String,
        p256dh: [u8; 65],
        auth: [u8; 16],
    },
    PushUnregister {
        proof: InboxProof,
    },
}

/// That the owner of `inbox` (an Ed25519 public key) made a request at
/// `timestamp` (unix seconds): its signature over what the request does
/// (`cypher_crypto::inbox`). Reading an inbox never hands the server a
/// secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboxProof {
    pub inbox: [u8; 32],
    pub timestamp: u64,
    pub signature: [u8; 64],
}

impl InboxProof {
    const LEN: usize = 32 + 8 + 64;

    fn put(&self, b: &mut BytesMut) {
        b.put_slice(&self.inbox);
        b.put_u64_le(self.timestamp);
        b.put_slice(&self.signature);
    }

    fn read(r: &mut Reader) -> Result<Self, WireError> {
        Ok(Self {
            inbox: r.array()?,
            timestamp: u64::from_le_bytes(r.array()?),
            signature: r.array()?,
        })
    }
}

/// Server → client messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMsg {
    Challenge {
        nonce: [u8; 32],
    },
    Ready,
    Pong,
    /// A relayed body; `from` and `device` are stamped by the gateway from
    /// the authenticated session and cannot be forged by the sender.
    Recv {
        from: PeerId,
        device: DeviceId,
        body: Bytes,
    },
    SendAck {
        status: DeliveryStatus,
    },
    /// An item delivered sealed: the gateway does not know who sent it.
    RecvSealed {
        item: Bytes,
    },
    Keys {
        base: Bytes,
        opk: Option<(u32, [u8; 32])>,
    },
    KeysAck {
        opks_left: u16,
    },
    /// A signed device list, as its identity published it.
    Devices {
        list: Bytes,
    },
    LinkCreated {
        link: LinkId,
    },
    LinkResolved {
        peer: PeerId,
    },
    InboxBatch {
        claim: [u8; 16],
        items: Vec<Bytes>,
    },
    Done,
    /// Where the onion relay is, and the long-term key the server signs its
    /// onion keys with.
    BootstrapInfo {
        relay_addr: String,
        signing_key: [u8; 32],
        capabilities: u32,
    },
    /// Uncompressed P-256 VAPID public key.
    PushKey {
        key: [u8; 65],
    },
    Error {
        code: ErrorCode,
    },
    /// The same device of this identity authenticated on another
    /// connection.
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DeliveryStatus {
    Delivered = 0,
    Offline = 1,
    Busy = 2,
    /// A sealed send left in the device's inbox while it is away.
    Queued = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    NotFound = 1,
    BadRequest = 2,
    RateLimited = 3,
    Unauthorized = 4,
    TooLarge = 5,
    Unavailable = 6,
    Internal = 7,
    /// The server does not speak the client's protocol version.
    UnsupportedVersion = 8,
    /// A newer version of what the client tried to replace is already
    /// stored.
    Conflict = 9,
}

mod kind {
    pub(super) const HELLO: u8 = 0x01;
    pub(super) const CHALLENGE: u8 = 0x02;
    pub(super) const AUTH: u8 = 0x03;
    pub(super) const READY: u8 = 0x04;
    pub(super) const PING: u8 = 0x05;
    pub(super) const PONG: u8 = 0x06;
    pub(super) const SUPERSEDED: u8 = 0x07;
    pub(super) const SEND: u8 = 0x10;
    pub(super) const RECV: u8 = 0x11;
    pub(super) const SEND_ACK: u8 = 0x12;
    pub(super) const SEND_SEALED: u8 = 0x13;
    pub(super) const RECV_SEALED: u8 = 0x14;
    pub(super) const SET_ACCESS: u8 = 0x15;
    pub(super) const PUBLISH_KEYS: u8 = 0x20;
    pub(super) const FETCH_KEYS: u8 = 0x21;
    pub(super) const KEYS: u8 = 0x22;
    pub(super) const KEYS_ACK: u8 = 0x23;
    pub(super) const PUBLISH_DEVICES: u8 = 0x24;
    pub(super) const FETCH_DEVICES: u8 = 0x25;
    pub(super) const DEVICES: u8 = 0x26;
    pub(super) const CREATE_LINK: u8 = 0x30;
    pub(super) const RESOLVE_LINK: u8 = 0x31;
    pub(super) const LINK_CREATED: u8 = 0x32;
    pub(super) const LINK_RESOLVED: u8 = 0x33;
    pub(super) const INBOX_PUT: u8 = 0x40;
    pub(super) const INBOX_FETCH: u8 = 0x41;
    pub(super) const INBOX_ACK: u8 = 0x42;
    pub(super) const INBOX_BATCH: u8 = 0x43;
    pub(super) const DONE: u8 = 0x44;
    pub(super) const BOOTSTRAP: u8 = 0x50;
    pub(super) const BOOTSTRAP_INFO: u8 = 0x51;
    pub(super) const PUSH_KEY: u8 = 0x60;
    pub(super) const PUSH_REGISTER: u8 = 0x61;
    pub(super) const PUSH_UNREGISTER: u8 = 0x62;
    pub(super) const ERROR: u8 = 0x7F;
}

fn header(kind: u8, req_id: u32, body_len: usize) -> BytesMut {
    let mut b = BytesMut::with_capacity(body_len.saturating_add(FRAME_HEADER_LEN));
    b.put_u8(kind);
    b.put_u32_le(req_id);
    b
}

fn read_header(buf: Bytes) -> Result<(u8, u32, Reader), WireError> {
    let mut r = Reader::new(buf);
    let kind = r.u8()?;
    let req_id = r.u32()?;
    Ok((kind, req_id, r))
}

fn read_opk(r: &mut Reader) -> Result<Option<(u32, [u8; 32])>, WireError> {
    match r.u8()? {
        0 => Ok(None),
        1 => Ok(Some((r.u32()?, r.array()?))),
        _ => Err(WireError::Malformed),
    }
}

fn read_bool(r: &mut Reader) -> Result<bool, WireError> {
    match r.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(WireError::Malformed),
    }
}

fn read_link(r: &mut Reader) -> Result<LinkId, WireError> {
    let raw = r.array::<{ LinkId::ENCODED_LEN }>()?;
    std::str::from_utf8(&raw)
        .ok()
        .and_then(LinkId::parse)
        .ok_or(WireError::Malformed)
}

impl Frame<ClientMsg> {
    #[expect(clippy::too_many_lines, reason = "one flat arm per message kind")]
    pub fn encode(&self) -> Bytes {
        use ClientMsg as M;
        let id = self.req_id;
        let b = match &self.msg {
            M::Hello {
                version,
                peer,
                device,
            } => {
                let mut b = header(kind::HELLO, id, 38);
                b.put_u16_le(*version);
                b.put_slice(peer.as_bytes());
                b.put_u32_le(device.0);
                b
            }
            M::Auth { signature } => {
                let mut b = header(kind::AUTH, id, 64);
                b.put_slice(signature);
                b
            }
            M::Ping => header(kind::PING, id, 0),
            M::Send {
                to,
                device,
                want_ack,
                body,
            } => {
                let mut b = header(kind::SEND, id, body.len().saturating_add(37));
                b.put_slice(to.as_bytes());
                b.put_u32_le(device.0);
                b.put_u8(u8::from(*want_ack));
                b.put_slice(body);
                b
            }
            M::SendSealed {
                to,
                device,
                token,
                inbox,
                item,
            } => {
                let mut b = header(kind::SEND_SEALED, id, item.len().saturating_add(101));
                b.put_slice(to.as_bytes());
                b.put_u32_le(device.0);
                b.put_slice(token);
                match inbox {
                    Some(inbox) => {
                        b.put_u8(1);
                        b.put_slice(inbox);
                    }
                    None => b.put_u8(0),
                }
                b.put_slice(item);
                b
            }
            M::SetAccess { hashes } => {
                let mut b = header(
                    kind::SET_ACCESS,
                    id,
                    hashes.len().saturating_mul(32).saturating_add(1),
                );
                b.put_u8(field_len(hashes.len()));
                for hash in hashes {
                    b.put_slice(hash);
                }
                b
            }
            M::PublishKeys {
                base,
                opks,
                replace_opks,
            } => {
                let mut b = header(
                    kind::PUBLISH_KEYS,
                    id,
                    opks.len()
                        .saturating_mul(36)
                        .saturating_add(base.len())
                        .saturating_add(3),
                );
                b.put_slice(base);
                b.put_u8(u8::from(*replace_opks));
                b.put_u16_le(field_len(opks.len()));
                for (opk_id, key) in opks {
                    b.put_u32_le(*opk_id);
                    b.put_slice(key);
                }
                b
            }
            M::FetchKeys { peer, device } => {
                let mut b = header(kind::FETCH_KEYS, id, 36);
                b.put_slice(peer.as_bytes());
                b.put_u32_le(device.0);
                b
            }
            M::PublishDevices { list } => {
                let mut b = header(kind::PUBLISH_DEVICES, id, list.len().saturating_add(4));
                b.put_bytes_prefixed(list);
                b
            }
            M::FetchDevices { peer } => {
                let mut b = header(kind::FETCH_DEVICES, id, 32);
                b.put_slice(peer.as_bytes());
                b
            }
            M::CreateLink => header(kind::CREATE_LINK, id, 0),
            M::ResolveLink { link } => {
                let mut b = header(kind::RESOLVE_LINK, id, LinkId::ENCODED_LEN);
                b.put_slice(link.as_str().as_bytes());
                b
            }
            M::InboxPut { inbox, item } => {
                let mut b = header(kind::INBOX_PUT, id, item.len().saturating_add(32));
                b.put_slice(inbox);
                b.put_slice(item);
                b
            }
            M::InboxFetch { proof, close } => {
                let mut b = header(kind::INBOX_FETCH, id, InboxProof::LEN + 1);
                proof.put(&mut b);
                b.put_u8(u8::from(*close));
                b
            }
            M::InboxAck { proof, claim } => {
                let mut b = header(kind::INBOX_ACK, id, InboxProof::LEN + 16);
                proof.put(&mut b);
                b.put_slice(claim);
                b
            }
            M::Bootstrap => header(kind::BOOTSTRAP, id, 0),
            M::PushKey => header(kind::PUSH_KEY, id, 0),
            M::PushRegister {
                proof,
                endpoint,
                p256dh,
                auth,
            } => {
                let len = endpoint.len().saturating_add(InboxProof::LEN + 85);
                let mut b = header(kind::PUSH_REGISTER, id, len);
                proof.put(&mut b);
                b.put_slice(p256dh);
                b.put_slice(auth);
                b.put_bytes_prefixed(endpoint.as_bytes());
                b
            }
            M::PushUnregister { proof } => {
                let mut b = header(kind::PUSH_UNREGISTER, id, InboxProof::LEN);
                proof.put(&mut b);
                b
            }
        };
        b.freeze()
    }

    #[expect(
        clippy::too_many_lines,
        clippy::cognitive_complexity,
        reason = "one flat arm per message kind"
    )]
    pub fn decode(buf: Bytes) -> Result<Self, WireError> {
        use ClientMsg as M;
        let (k, req_id, mut r) = read_header(buf)?;
        let msg = match k {
            kind::HELLO => M::Hello {
                version: r.u16()?,
                peer: PeerId(r.array()?),
                device: r.device()?,
            },
            kind::AUTH => M::Auth {
                signature: r.array()?,
            },
            kind::PING => M::Ping,
            kind::SEND => {
                let to = PeerId(r.array()?);
                let device = r.device()?;
                let want_ack = read_bool(&mut r)?;
                let body = r.rest();
                if body.len() > MAX_BODY_LEN {
                    return Err(WireError::TooLarge);
                }
                let send = M::Send {
                    to,
                    device,
                    want_ack,
                    body,
                };
                return Ok(Self::new(req_id, send));
            }
            kind::SEND_SEALED => {
                let to = PeerId(r.array()?);
                let device = r.device()?;
                let token = r.array()?;
                let inbox = if read_bool(&mut r)? {
                    Some(r.array()?)
                } else {
                    None
                };
                let item = r.rest();
                if item.len() > MAX_INBOX_ITEM_LEN {
                    return Err(WireError::TooLarge);
                }
                let send = M::SendSealed {
                    to,
                    device,
                    token,
                    inbox,
                    item,
                };
                return Ok(Self::new(req_id, send));
            }
            kind::SET_ACCESS => {
                let count = r.u8()? as usize;
                if count > MAX_ACCESS_HASHES {
                    return Err(WireError::TooLarge);
                }
                M::SetAccess {
                    hashes: (0..count).map(|_| r.array()).collect::<Result<_, _>>()?,
                }
            }
            kind::PUBLISH_KEYS => {
                let base = Bytes::copy_from_slice(&r.array::<BUNDLE_BASE_LEN>()?);
                let replace_opks = read_bool(&mut r)?;
                let count = r.u16()? as usize;
                if count > MAX_OPKS_PER_PUBLISH {
                    return Err(WireError::TooLarge);
                }
                let opks = (0..count)
                    .map(|_| Ok((r.u32()?, r.array()?)))
                    .collect::<Result<_, WireError>>()?;
                M::PublishKeys {
                    base,
                    opks,
                    replace_opks,
                }
            }
            kind::FETCH_KEYS => M::FetchKeys {
                peer: PeerId(r.array()?),
                device: r.device()?,
            },
            kind::PUBLISH_DEVICES => M::PublishDevices {
                list: r.bytes(MAX_DEVICE_LIST_LEN)?,
            },
            kind::FETCH_DEVICES => M::FetchDevices {
                peer: PeerId(r.array()?),
            },
            kind::CREATE_LINK => M::CreateLink,
            kind::RESOLVE_LINK => M::ResolveLink {
                link: read_link(&mut r)?,
            },
            kind::INBOX_PUT => {
                let inbox = r.array()?;
                let item = r.rest();
                if item.len() > MAX_INBOX_ITEM_LEN {
                    return Err(WireError::TooLarge);
                }
                return Ok(Self::new(req_id, M::InboxPut { inbox, item }));
            }
            kind::INBOX_FETCH => M::InboxFetch {
                proof: InboxProof::read(&mut r)?,
                close: read_bool(&mut r)?,
            },
            kind::INBOX_ACK => M::InboxAck {
                proof: InboxProof::read(&mut r)?,
                claim: r.array()?,
            },
            kind::BOOTSTRAP => M::Bootstrap,
            kind::PUSH_KEY => M::PushKey,
            kind::PUSH_REGISTER => M::PushRegister {
                proof: InboxProof::read(&mut r)?,
                p256dh: r.array()?,
                auth: r.array()?,
                endpoint: String::from_utf8(r.bytes(MAX_PUSH_ENDPOINT_LEN)?.to_vec())
                    .map_err(|_| WireError::Malformed)?,
            },
            kind::PUSH_UNREGISTER => M::PushUnregister {
                proof: InboxProof::read(&mut r)?,
            },
            other => return Err(WireError::UnknownKind(other)),
        };
        r.finish()?;
        Ok(Self::new(req_id, msg))
    }
}

impl Frame<ServerMsg> {
    #[expect(clippy::too_many_lines, reason = "one flat arm per message kind")]
    pub fn encode(&self) -> Bytes {
        use ServerMsg as M;
        let id = self.req_id;
        let b = match &self.msg {
            M::Challenge { nonce } => {
                let mut b = header(kind::CHALLENGE, id, 32);
                b.put_slice(nonce);
                b
            }
            M::Ready => header(kind::READY, id, 0),
            M::Pong => header(kind::PONG, id, 0),
            M::Recv { from, device, body } => return recv_frame(id, from, *device, body),
            M::SendAck { status } => {
                let mut b = header(kind::SEND_ACK, id, 1);
                b.put_u8(*status as u8);
                b
            }
            M::RecvSealed { item } => return recv_sealed_frame(id, item),
            M::Keys { base, opk } => {
                let mut b = header(kind::KEYS, id, base.len().saturating_add(37));
                b.put_slice(base);
                match opk {
                    Some((opk_id, key)) => {
                        b.put_u8(1);
                        b.put_u32_le(*opk_id);
                        b.put_slice(key);
                    }
                    None => b.put_u8(0),
                }
                b
            }
            M::KeysAck { opks_left } => {
                let mut b = header(kind::KEYS_ACK, id, 2);
                b.put_u16_le(*opks_left);
                b
            }
            M::Devices { list } => {
                let mut b = header(kind::DEVICES, id, list.len().saturating_add(4));
                b.put_bytes_prefixed(list);
                b
            }
            M::LinkCreated { link } => {
                let mut b = header(kind::LINK_CREATED, id, LinkId::ENCODED_LEN);
                b.put_slice(link.as_str().as_bytes());
                b
            }
            M::LinkResolved { peer } => {
                let mut b = header(kind::LINK_RESOLVED, id, 32);
                b.put_slice(peer.as_bytes());
                b
            }
            M::InboxBatch { claim, items } => {
                let body_len = items
                    .iter()
                    .fold(0usize, |n, i| n.saturating_add(i.len()).saturating_add(4));
                let mut b = header(kind::INBOX_BATCH, id, body_len.saturating_add(17));
                b.put_slice(claim);
                b.put_u8(field_len(items.len()));
                for item in items {
                    b.put_bytes_prefixed(item);
                }
                b
            }
            M::Done => header(kind::DONE, id, 0),
            M::BootstrapInfo {
                relay_addr,
                signing_key,
                capabilities,
            } => {
                let mut b = header(
                    kind::BOOTSTRAP_INFO,
                    id,
                    relay_addr.len().saturating_add(37),
                );
                b.put_short_str(relay_addr);
                b.put_slice(signing_key);
                b.put_u32_le(*capabilities);
                b
            }
            M::PushKey { key } => {
                let mut b = header(kind::PUSH_KEY, id, 65);
                b.put_slice(key);
                b
            }
            M::Error { code } => {
                let mut b = header(kind::ERROR, id, 2);
                b.put_u16_le(*code as u16);
                b
            }
            M::Superseded => header(kind::SUPERSEDED, id, 0),
        };
        b.freeze()
    }

    #[expect(clippy::too_many_lines, reason = "one flat arm per message kind")]
    pub fn decode(buf: Bytes) -> Result<Self, WireError> {
        use ServerMsg as M;
        let (k, req_id, mut r) = read_header(buf)?;
        let msg = match k {
            kind::CHALLENGE => M::Challenge { nonce: r.array()? },
            kind::READY => M::Ready,
            kind::PONG => M::Pong,
            kind::RECV => {
                let from = PeerId(r.array()?);
                let device = r.device()?;
                let recv = M::Recv {
                    from,
                    device,
                    body: r.rest(),
                };
                return Ok(Self::new(req_id, recv));
            }
            kind::SEND_ACK => M::SendAck {
                status: match r.u8()? {
                    0 => DeliveryStatus::Delivered,
                    1 => DeliveryStatus::Offline,
                    2 => DeliveryStatus::Busy,
                    3 => DeliveryStatus::Queued,
                    _ => return Err(WireError::Malformed),
                },
            },
            kind::RECV_SEALED => {
                let item = r.rest();
                if item.len() > MAX_INBOX_ITEM_LEN {
                    return Err(WireError::TooLarge);
                }
                return Ok(Self::new(req_id, M::RecvSealed { item }));
            }
            kind::KEYS => M::Keys {
                base: Bytes::copy_from_slice(&r.array::<BUNDLE_BASE_LEN>()?),
                opk: read_opk(&mut r)?,
            },
            kind::KEYS_ACK => M::KeysAck {
                opks_left: r.u16()?,
            },
            kind::DEVICES => M::Devices {
                list: r.bytes(MAX_DEVICE_LIST_LEN)?,
            },
            kind::LINK_CREATED => M::LinkCreated {
                link: read_link(&mut r)?,
            },
            kind::LINK_RESOLVED => M::LinkResolved {
                peer: PeerId(r.array()?),
            },
            kind::INBOX_BATCH => {
                let claim = r.array()?;
                let count = r.u8()? as usize;
                if count > MAX_INBOX_BATCH {
                    return Err(WireError::TooLarge);
                }
                let items = (0..count)
                    .map(|_| r.bytes(MAX_INBOX_ITEM_LEN))
                    .collect::<Result<_, _>>()?;
                M::InboxBatch { claim, items }
            }
            kind::DONE => M::Done,
            kind::BOOTSTRAP_INFO => M::BootstrapInfo {
                relay_addr: r.short_str()?,
                signing_key: r.array()?,
                capabilities: r.u32()?,
            },
            kind::PUSH_KEY => M::PushKey { key: r.array()? },
            kind::ERROR => M::Error {
                code: match r.u16()? {
                    1 => ErrorCode::NotFound,
                    2 => ErrorCode::BadRequest,
                    3 => ErrorCode::RateLimited,
                    4 => ErrorCode::Unauthorized,
                    5 => ErrorCode::TooLarge,
                    6 => ErrorCode::Unavailable,
                    8 => ErrorCode::UnsupportedVersion,
                    9 => ErrorCode::Conflict,
                    _ => ErrorCode::Internal,
                },
            },
            kind::SUPERSEDED => M::Superseded,
            other => return Err(WireError::UnknownKind(other)),
        };
        r.finish()?;
        Ok(Self::new(req_id, msg))
    }
}

/// Hot-path encoder used by the gateway: one allocation, one body copy.
pub fn encode_recv(from: &PeerId, device: DeviceId, body: &[u8]) -> Bytes {
    recv_frame(0, from, device, body)
}

fn recv_frame(req_id: u32, from: &PeerId, device: DeviceId, body: &[u8]) -> Bytes {
    let mut b = header(kind::RECV, req_id, body.len().saturating_add(36));
    b.put_slice(from.as_bytes());
    b.put_u32_le(device.0);
    b.put_slice(body);
    b.freeze()
}

/// The unsolicited frame that hands a device an item delivered sealed;
/// the gateway builds it straight from what it was relayed.
pub fn encode_recv_sealed(item: &[u8]) -> Bytes {
    recv_sealed_frame(0, item)
}

fn recv_sealed_frame(req_id: u32, item: &[u8]) -> Bytes {
    let mut b = header(kind::RECV_SEALED, req_id, item.len());
    b.put_slice(item);
    b.freeze()
}

/// `[kind][req_id][to 32][device 4][want_ack]` in front of a client `Send`
/// body.
pub const SEND_HEADER_LEN: usize = FRAME_HEADER_LEN + 37;

/// A client `Send` as the gateway router sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendView {
    pub req_id: u32,
    pub to: PeerId,
    pub device: DeviceId,
    pub want_ack: bool,
    pub body: Bytes,
}

/// Zero-copy view of a client `Send` used by the gateway router: peeks at the
/// destination without decoding the whole frame.
pub fn peek_send(frame: &Bytes) -> Option<SendView> {
    let (&[k], rest) = frame.split_first_chunk::<1>()?;
    let (req_id, rest) = rest.split_first_chunk::<4>()?;
    let (to, rest) = rest.split_first_chunk::<32>()?;
    let (device, rest) = rest.split_first_chunk::<4>()?;
    let (&[ack], body) = rest.split_first_chunk::<1>()?;
    let want_ack = match ack {
        0 => false,
        1 => true,
        _ => return None,
    };
    let device = DeviceId(u32::from_le_bytes(*device));
    if k != kind::SEND || !device.is_valid() || body.len() > MAX_BODY_LEN {
        return None;
    }
    Some(SendView {
        req_id: u32::from_le_bytes(*req_id),
        to: PeerId(*to),
        device,
        want_ack,
        body: frame.slice(SEND_HEADER_LEN..),
    })
}

/// Validated relay address for [`ServerMsg::BootstrapInfo`].
pub fn relay_addr_is_valid(addr: &str) -> bool {
    !addr.is_empty() && addr.len() <= MAX_RELAY_ADDR_LEN
}
