//! Layout of the opaque `body` relayed by the server between peers.

use bytes::{BufMut, Bytes, BytesMut};
use cypher_crypto::chunk::ACK_TAG_LEN;
use cypher_crypto::double_ratchet::HEADER_LEN;
use cypher_crypto::{Header, InitHeader};
use cypher_types::{FileId, PeerId};

use crate::CoreError;

const TAG_MESSAGE: u8 = 0;
const TAG_CHUNK: u8 = 1;
const TAG_ACK: u8 = 2;

/// Bytes of `ClientMsg::Send` framing before the relay body.
pub const SEND_HEADER_LEN: usize = 38;
pub const CHUNK_HEADER_LEN: usize = 1 + 16 + 4;
/// Space a driver must reserve in front of chunk data read from disk.
pub const CHUNK_HEADROOM: usize = SEND_HEADER_LEN + CHUNK_HEADER_LEN;

#[derive(Debug, PartialEq, Eq)]
pub enum RelayBody {
    Message {
        init: Option<InitHeader>,
        header: Header,
        ciphertext: Bytes,
    },
    Chunk {
        file_id: FileId,
        index: u32,
        data: Bytes,
    },
    Ack {
        file_id: FileId,
        next: u32,
        sack: u64,
        tag: [u8; ACK_TAG_LEN],
    },
}

impl RelayBody {
    pub fn decode(body: Bytes) -> Result<Self, CoreError> {
        let (&tag, _) = body.split_first().ok_or(CoreError::Invalid)?;
        match tag {
            TAG_MESSAGE => {
                let flags = *body.get(1).ok_or(CoreError::Invalid)?;
                let mut rest = &body[2..];
                let init = match flags {
                    0 => None,
                    1 => {
                        let (init, tail) =
                            InitHeader::decode_prefix(rest).map_err(|_| CoreError::Invalid)?;
                        rest = tail;
                        Some(init)
                    }
                    _ => return Err(CoreError::Invalid),
                };
                if rest.len() < HEADER_LEN {
                    return Err(CoreError::Invalid);
                }
                let header = Header::decode(&rest[..HEADER_LEN]).map_err(|_| CoreError::Invalid)?;
                let offset = body.len() - rest.len() + HEADER_LEN;
                Ok(Self::Message {
                    init,
                    header,
                    ciphertext: body.slice(offset..),
                })
            }
            TAG_CHUNK => {
                if body.len() < CHUNK_HEADER_LEN {
                    return Err(CoreError::Invalid);
                }
                Ok(Self::Chunk {
                    file_id: FileId(array(&body[1..17])),
                    index: u32::from_le_bytes(array(&body[17..21])),
                    data: body.slice(CHUNK_HEADER_LEN..),
                })
            }
            TAG_ACK => {
                let b: &[u8; 1 + 16 + 4 + 8 + ACK_TAG_LEN] =
                    body[..].try_into().map_err(|_| CoreError::Invalid)?;
                Ok(Self::Ack {
                    file_id: FileId(array(&b[1..17])),
                    next: u32::from_le_bytes(array(&b[17..21])),
                    sack: u64::from_le_bytes(array(&b[21..29])),
                    tag: array(&b[29..45]),
                })
            }
            _ => Err(CoreError::Invalid),
        }
    }
}

/// Copies a length-checked slice into an array.
fn array<const N: usize>(s: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(s);
    out
}

/// Builds a full `ClientMsg::Send` frame for a ratchet message.
pub fn message_frame(
    req_id: u32,
    to: &PeerId,
    init: Option<&InitHeader>,
    header: &Header,
    ciphertext: &[u8],
) -> Bytes {
    let body = message_body(init, header, ciphertext);
    let mut b = BytesMut::with_capacity(SEND_HEADER_LEN + body.len());
    put_send_header(&mut b, req_id, to, true);
    b.put_slice(&body);
    b.freeze()
}

/// Relay body (without `Send` framing) of a ratchet message, as stored in a
/// peer's inbox.
pub fn message_body(init: Option<&InitHeader>, header: &Header, ciphertext: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(2 + InitHeader::MAX_LEN + HEADER_LEN + ciphertext.len());
    b.push(TAG_MESSAGE);
    match init {
        Some(init) => {
            b.push(1);
            init.encode(&mut b);
        }
        None => b.push(0),
    }
    b.extend_from_slice(&header.encode());
    b.extend_from_slice(ciphertext);
    b
}

/// Writes `Send` + chunk headers into the reserved headroom of `buf`.
pub fn write_chunk_headers(buf: &mut [u8], to: &PeerId, file_id: &FileId, index: u32) {
    let (send, chunk) = buf[..CHUNK_HEADROOM].split_at_mut(SEND_HEADER_LEN);
    write_send_header(send, 0, to, false);
    chunk[0] = TAG_CHUNK;
    chunk[1..17].copy_from_slice(file_id.as_bytes());
    chunk[17..21].copy_from_slice(&index.to_le_bytes());
}

pub fn ack_frame(
    to: &PeerId,
    file_id: &FileId,
    next: u32,
    sack: u64,
    tag: &[u8; ACK_TAG_LEN],
) -> Bytes {
    let mut b = BytesMut::with_capacity(SEND_HEADER_LEN + 45);
    put_send_header(&mut b, 0, to, false);
    b.put_u8(TAG_ACK);
    b.put_slice(file_id.as_bytes());
    b.put_u32_le(next);
    b.put_u64_le(sack);
    b.put_slice(tag);
    b.freeze()
}

fn put_send_header(b: &mut BytesMut, req_id: u32, to: &PeerId, want_ack: bool) {
    let mut hdr = [0u8; SEND_HEADER_LEN];
    write_send_header(&mut hdr, req_id, to, want_ack);
    b.put_slice(&hdr);
}

fn write_send_header(out: &mut [u8], req_id: u32, to: &PeerId, want_ack: bool) {
    const KIND_SEND: u8 = 0x10;
    out[0] = KIND_SEND;
    out[1..5].copy_from_slice(&req_id.to_le_bytes());
    out[5..37].copy_from_slice(to.as_bytes());
    out[37] = u8::from(want_ack);
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_wire::{ClientMsg, Frame};

    fn decode_send(frame: Bytes) -> (PeerId, Bytes) {
        match Frame::<ClientMsg>::decode(frame).unwrap().msg {
            ClientMsg::Send { to, body, .. } => (to, body),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn message_frame_is_a_valid_send_and_roundtrips() {
        let header = Header {
            dh: [1; 32],
            pn: 2,
            n: 3,
        };
        let init = InitHeader {
            identity_dh: [4; 32],
            ephemeral: [5; 32],
            spk_id: 6,
            opk_id: Some(7),
            signature: [8; 64],
        };
        for init in [None, Some(init)] {
            let frame = message_frame(9, &PeerId([10; 32]), init.as_ref(), &header, b"ct");
            let (to, body) = decode_send(frame);
            assert_eq!(to, PeerId([10; 32]));
            assert_eq!(&body[..], &message_body(init.as_ref(), &header, b"ct")[..]);
            assert_eq!(
                RelayBody::decode(body).unwrap(),
                RelayBody::Message {
                    init,
                    header,
                    ciphertext: Bytes::from_static(b"ct")
                }
            );
        }
    }

    #[test]
    fn chunk_headers_written_in_place() {
        let mut buf = vec![0u8; CHUNK_HEADROOM + 3];
        buf[CHUNK_HEADROOM..].copy_from_slice(b"abc");
        write_chunk_headers(&mut buf, &PeerId([1; 32]), &FileId([2; 16]), 5);
        let (_, body) = decode_send(Bytes::from(buf));
        assert_eq!(
            RelayBody::decode(body).unwrap(),
            RelayBody::Chunk {
                file_id: FileId([2; 16]),
                index: 5,
                data: Bytes::from_static(b"abc")
            }
        );
    }

    #[test]
    fn ack_roundtrip_and_garbage() {
        let (_, body) = decode_send(ack_frame(
            &PeerId([1; 32]),
            &FileId([3; 16]),
            4,
            5,
            &[6; 16],
        ));
        assert_eq!(
            RelayBody::decode(body).unwrap(),
            RelayBody::Ack {
                file_id: FileId([3; 16]),
                next: 4,
                sack: 5,
                tag: [6; 16]
            }
        );
        for bad in [&b""[..], &[0], &[0, 2], &[1, 0], &[2; 10], &[9]] {
            assert!(RelayBody::decode(Bytes::copy_from_slice(bad)).is_err());
        }
    }
}
