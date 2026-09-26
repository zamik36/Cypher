use bytes::{BufMut, Bytes, BytesMut};
use cypher_types::{LinkId, PeerId};

use crate::codec::{Reader, WriteExt};
use crate::{
    BUNDLE_BASE_LEN, FRAME_HEADER_LEN, MAX_BODY_LEN, MAX_INBOX_BATCH, MAX_INBOX_ITEM_LEN,
    MAX_OPKS_PER_PUBLISH, MAX_RELAY_ADDR_LEN, WireError,
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
    Hello {
        version: u16,
        peer: PeerId,
    },
    Auth {
        signature: [u8; 64],
    },
    Ping,
    /// Relay an opaque end-to-end body to `to`. With `want_ack` the server
    /// answers with [`ServerMsg::SendAck`].
    Send {
        to: PeerId,
        want_ack: bool,
        body: Bytes,
    },
    /// `base` is a [`BUNDLE_BASE_LEN`]-byte signed prekey bundle.
    PublishKeys {
        base: Bytes,
        opks: Vec<(u32, [u8; 32])>,
        replace_opks: bool,
    },
    FetchKeys {
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
    /// The inbox is addressed by `H(secret)`; knowing only the id is not
    /// enough to read or delete its contents.
    InboxFetch {
        secret: [u8; 32],
    },
    InboxAck {
        secret: [u8; 32],
        claim: [u8; 16],
    },
    Bootstrap,
}

/// Server → client messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMsg {
    Challenge {
        nonce: [u8; 32],
    },
    Ready,
    Pong,
    /// A relayed body; `from` is stamped by the gateway from the
    /// authenticated session and cannot be forged by the sender.
    Recv {
        from: PeerId,
        body: Bytes,
    },
    SendAck {
        status: DeliveryStatus,
    },
    Keys {
        base: Bytes,
        opk: Option<(u32, [u8; 32])>,
    },
    KeysAck {
        opks_left: u16,
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
    BootstrapInfo {
        relay_addr: String,
        relay_key: [u8; 32],
        capabilities: u32,
    },
    Error {
        code: ErrorCode,
    },
    /// The same identity authenticated on another connection.
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DeliveryStatus {
    Delivered = 0,
    Offline = 1,
    Busy = 2,
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
}

mod kind {
    pub const HELLO: u8 = 0x01;
    pub const CHALLENGE: u8 = 0x02;
    pub const AUTH: u8 = 0x03;
    pub const READY: u8 = 0x04;
    pub const PING: u8 = 0x05;
    pub const PONG: u8 = 0x06;
    pub const SUPERSEDED: u8 = 0x07;
    pub const SEND: u8 = 0x10;
    pub const RECV: u8 = 0x11;
    pub const SEND_ACK: u8 = 0x12;
    pub const PUBLISH_KEYS: u8 = 0x20;
    pub const FETCH_KEYS: u8 = 0x21;
    pub const KEYS: u8 = 0x22;
    pub const KEYS_ACK: u8 = 0x23;
    pub const CREATE_LINK: u8 = 0x30;
    pub const RESOLVE_LINK: u8 = 0x31;
    pub const LINK_CREATED: u8 = 0x32;
    pub const LINK_RESOLVED: u8 = 0x33;
    pub const INBOX_PUT: u8 = 0x40;
    pub const INBOX_FETCH: u8 = 0x41;
    pub const INBOX_ACK: u8 = 0x42;
    pub const INBOX_BATCH: u8 = 0x43;
    pub const DONE: u8 = 0x44;
    pub const BOOTSTRAP: u8 = 0x50;
    pub const BOOTSTRAP_INFO: u8 = 0x51;
    pub const ERROR: u8 = 0x7F;
}

fn header(kind: u8, req_id: u32, body_len: usize) -> BytesMut {
    let mut b = BytesMut::with_capacity(FRAME_HEADER_LEN + body_len);
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
    pub fn encode(&self) -> Bytes {
        use ClientMsg as M;
        let id = self.req_id;
        let b = match &self.msg {
            M::Hello { version, peer } => {
                let mut b = header(kind::HELLO, id, 34);
                b.put_u16_le(*version);
                b.put_slice(peer.as_bytes());
                b
            }
            M::Auth { signature } => {
                let mut b = header(kind::AUTH, id, 64);
                b.put_slice(signature);
                b
            }
            M::Ping => header(kind::PING, id, 0),
            M::Send { to, want_ack, body } => {
                let mut b = header(kind::SEND, id, 33 + body.len());
                b.put_slice(to.as_bytes());
                b.put_u8(u8::from(*want_ack));
                b.put_slice(body);
                b
            }
            M::PublishKeys {
                base,
                opks,
                replace_opks,
            } => {
                let mut b = header(kind::PUBLISH_KEYS, id, base.len() + 3 + opks.len() * 36);
                b.put_slice(base);
                b.put_u8(u8::from(*replace_opks));
                b.put_u16_le(u16::try_from(opks.len()).expect("bounded by MAX_OPKS_PER_PUBLISH"));
                for (opk_id, key) in opks {
                    b.put_u32_le(*opk_id);
                    b.put_slice(key);
                }
                b
            }
            M::FetchKeys { peer } => {
                let mut b = header(kind::FETCH_KEYS, id, 32);
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
                let mut b = header(kind::INBOX_PUT, id, 32 + item.len());
                b.put_slice(inbox);
                b.put_slice(item);
                b
            }
            M::InboxFetch { secret } => {
                let mut b = header(kind::INBOX_FETCH, id, 32);
                b.put_slice(secret);
                b
            }
            M::InboxAck { secret, claim } => {
                let mut b = header(kind::INBOX_ACK, id, 48);
                b.put_slice(secret);
                b.put_slice(claim);
                b
            }
            M::Bootstrap => header(kind::BOOTSTRAP, id, 0),
        };
        b.freeze()
    }

    pub fn decode(buf: Bytes) -> Result<Self, WireError> {
        use ClientMsg as M;
        let (k, req_id, mut r) = read_header(buf)?;
        let msg = match k {
            kind::HELLO => M::Hello {
                version: r.u16()?,
                peer: PeerId(r.array()?),
            },
            kind::AUTH => M::Auth {
                signature: r.array()?,
            },
            kind::PING => M::Ping,
            kind::SEND => {
                let to = PeerId(r.array()?);
                let want_ack = read_bool(&mut r)?;
                let body = r.rest();
                if body.len() > MAX_BODY_LEN {
                    return Err(WireError::TooLarge);
                }
                return Ok(Self::new(req_id, M::Send { to, want_ack, body }));
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
            kind::INBOX_FETCH => M::InboxFetch { secret: r.array()? },
            kind::INBOX_ACK => M::InboxAck {
                secret: r.array()?,
                claim: r.array()?,
            },
            kind::BOOTSTRAP => M::Bootstrap,
            other => return Err(WireError::UnknownKind(other)),
        };
        r.finish()?;
        Ok(Self::new(req_id, msg))
    }
}

impl Frame<ServerMsg> {
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
            M::Recv { from, body } => return recv_frame(id, from, body),
            M::SendAck { status } => {
                let mut b = header(kind::SEND_ACK, id, 1);
                b.put_u8(*status as u8);
                b
            }
            M::Keys { base, opk } => {
                let mut b = header(kind::KEYS, id, base.len() + 37);
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
                let body_len: usize = items.iter().map(|i| 4 + i.len()).sum();
                let mut b = header(kind::INBOX_BATCH, id, 17 + body_len);
                b.put_slice(claim);
                b.put_u8(u8::try_from(items.len()).expect("bounded by MAX_INBOX_BATCH"));
                for item in items {
                    b.put_bytes_prefixed(item);
                }
                b
            }
            M::Done => header(kind::DONE, id, 0),
            M::BootstrapInfo {
                relay_addr,
                relay_key,
                capabilities,
            } => {
                let mut b = header(kind::BOOTSTRAP_INFO, id, 37 + relay_addr.len());
                b.put_short_str(relay_addr);
                b.put_slice(relay_key);
                b.put_u32_le(*capabilities);
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

    pub fn decode(buf: Bytes) -> Result<Self, WireError> {
        use ServerMsg as M;
        let (k, req_id, mut r) = read_header(buf)?;
        let msg = match k {
            kind::CHALLENGE => M::Challenge { nonce: r.array()? },
            kind::READY => M::Ready,
            kind::PONG => M::Pong,
            kind::RECV => {
                let from = PeerId(r.array()?);
                return Ok(Self::new(
                    req_id,
                    M::Recv {
                        from,
                        body: r.rest(),
                    },
                ));
            }
            kind::SEND_ACK => M::SendAck {
                status: match r.u8()? {
                    0 => DeliveryStatus::Delivered,
                    1 => DeliveryStatus::Offline,
                    2 => DeliveryStatus::Busy,
                    _ => return Err(WireError::Malformed),
                },
            },
            kind::KEYS => M::Keys {
                base: Bytes::copy_from_slice(&r.array::<BUNDLE_BASE_LEN>()?),
                opk: read_opk(&mut r)?,
            },
            kind::KEYS_ACK => M::KeysAck {
                opks_left: r.u16()?,
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
                relay_key: r.array()?,
                capabilities: r.u32()?,
            },
            kind::ERROR => M::Error {
                code: match r.u16()? {
                    1 => ErrorCode::NotFound,
                    2 => ErrorCode::BadRequest,
                    3 => ErrorCode::RateLimited,
                    4 => ErrorCode::Unauthorized,
                    5 => ErrorCode::TooLarge,
                    6 => ErrorCode::Unavailable,
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
pub fn encode_recv(from: &PeerId, body: &[u8]) -> Bytes {
    recv_frame(0, from, body)
}

fn recv_frame(req_id: u32, from: &PeerId, body: &[u8]) -> Bytes {
    let mut b = header(kind::RECV, req_id, 32 + body.len());
    b.put_slice(from.as_bytes());
    b.put_slice(body);
    b.freeze()
}

/// Zero-copy view of a client `Send` used by the gateway router: peeks at the
/// destination without decoding the whole frame.
pub fn peek_send(frame: &Bytes) -> Option<(u32, PeerId, bool, Bytes)> {
    if frame.len() < FRAME_HEADER_LEN + 33 || frame[0] != kind::SEND {
        return None;
    }
    let req_id = u32::from_le_bytes(frame[1..5].try_into().ok()?);
    let to = PeerId(frame[5..37].try_into().ok()?);
    let want_ack = match frame[37] {
        0 => false,
        1 => true,
        _ => return None,
    };
    let body = frame.slice(38..);
    (body.len() <= MAX_BODY_LEN).then_some((req_id, to, want_ack, body))
}

/// Validated relay address for [`ServerMsg::BootstrapInfo`].
pub fn relay_addr_is_valid(addr: &str) -> bool {
    !addr.is_empty() && addr.len() <= MAX_RELAY_ADDR_LEN
}
