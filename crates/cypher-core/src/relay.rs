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
    pub fn decode(body: &Bytes) -> Result<Self, CoreError> {
        let (&tag, mut rest) = body.split_first().ok_or(CoreError::Invalid)?;
        match tag {
            TAG_MESSAGE => {
                let init = match take::<1>(&mut rest)? {
                    [0] => None,
                    [1] => {
                        let (init, tail) =
                            InitHeader::decode_prefix(rest).map_err(|_| CoreError::Invalid)?;
                        rest = tail;
                        Some(init)
                    }
                    _ => return Err(CoreError::Invalid),
                };
                let header = Header::decode(&take::<HEADER_LEN>(&mut rest)?)
                    .map_err(|_| CoreError::Invalid)?;
                Ok(Self::Message {
                    init,
                    header,
                    ciphertext: body.slice(body.len() - rest.len()..),
                })
            }
            TAG_CHUNK => {
                let file_id = FileId(take(&mut rest)?);
                let index = u32::from_le_bytes(take(&mut rest)?);
                Ok(Self::Chunk {
                    file_id,
                    index,
                    data: body.slice(CHUNK_HEADER_LEN..),
                })
            }
            TAG_ACK => {
                let ack = Self::Ack {
                    file_id: FileId(take(&mut rest)?),
                    next: u32::from_le_bytes(take(&mut rest)?),
                    sack: u64::from_le_bytes(take(&mut rest)?),
                    tag: take(&mut rest)?,
                };
                if rest.is_empty() {
                    Ok(ack)
                } else {
                    Err(CoreError::Invalid)
                }
            }
            _ => Err(CoreError::Invalid),
        }
    }
}

/// Splits a fixed-size field off the front of `rest`.
fn take<const N: usize>(rest: &mut &[u8]) -> Result<[u8; N], CoreError> {
    let (head, tail) = rest.split_first_chunk::<N>().ok_or(CoreError::Invalid)?;
    *rest = tail;
    Ok(*head)
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

/// Writes `Send` + chunk headers into the headroom reserved in front of a
/// chunk read from disk.
pub fn write_chunk_headers(
    headroom: &mut [u8; CHUNK_HEADROOM],
    to: &PeerId,
    file_id: &FileId,
    index: u32,
) {
    headroom[..SEND_HEADER_LEN].copy_from_slice(&send_header(0, to, false));
    headroom[SEND_HEADER_LEN] = TAG_CHUNK;
    headroom[SEND_HEADER_LEN + 1..SEND_HEADER_LEN + 17].copy_from_slice(file_id.as_bytes());
    headroom[SEND_HEADER_LEN + 17..].copy_from_slice(&index.to_le_bytes());
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
    b.put_slice(&send_header(req_id, to, want_ack));
}

/// `ClientMsg::Send` framing: `[kind][req_id][to][want_ack]`.
fn send_header(req_id: u32, to: &PeerId, want_ack: bool) -> [u8; SEND_HEADER_LEN] {
    const KIND_SEND: u8 = 0x10;
    let mut out = [0u8; SEND_HEADER_LEN];
    out[0] = KIND_SEND;
    out[1..5].copy_from_slice(&req_id.to_le_bytes());
    out[5..37].copy_from_slice(to.as_bytes());
    out[37] = u8::from(want_ack);
    out
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
                RelayBody::decode(&body).unwrap(),
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
        let headroom = buf.first_chunk_mut::<CHUNK_HEADROOM>().unwrap();
        write_chunk_headers(headroom, &PeerId([1; 32]), &FileId([2; 16]), 5);
        let (_, body) = decode_send(Bytes::from(buf));
        assert_eq!(
            RelayBody::decode(&body).unwrap(),
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
            RelayBody::decode(&body).unwrap(),
            RelayBody::Ack {
                file_id: FileId([3; 16]),
                next: 4,
                sack: 5,
                tag: [6; 16]
            }
        );
        for bad in [&b""[..], &[0], &[0, 2], &[1, 0], &[2; 10], &[9]] {
            RelayBody::decode(&Bytes::copy_from_slice(bad)).unwrap_err();
        }
    }
}
