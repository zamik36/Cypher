use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::frame::{Frame, FrameFlags};

/// Maximum frame size: 1 MB.
const MAX_FRAME_SIZE: u32 = 1_048_576;

/// Minimum frame body size: seq_no(4) + ack(4) + flags(1) = 9 bytes.
const HEADER_SIZE: usize = 9;

/// Codec for encoding and decoding [`Frame`] values on the wire.
///
/// Wire format:
/// ```text
/// [length: u32][seq_no: u32][ack: u32][flags: u8][payload: ...]
/// ```
///
/// `length` is the number of bytes that follow (seq_no + ack + flags + payload),
/// i.e. it does NOT include the 4-byte length field itself.
#[derive(Debug, Default)]
pub struct FrameCodec;

impl FrameCodec {
    pub fn new() -> Self {
        Self
    }
}

impl Decoder for FrameCodec {
    type Item = Frame;
    type Error = std::io::Error;

    fn decode(
        &mut self,
        src: &mut BytesMut,
    ) -> std::result::Result<Option<Self::Item>, Self::Error> {
        // Need at least 4 bytes for the length prefix.
        if src.len() < 4 {
            return Ok(None);
        }

        // Peek at the length without consuming.
        let length = u32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;

        if length < HEADER_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("frame body too small: {length}"),
            ));
        }

        if length > MAX_FRAME_SIZE as usize {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("frame too large: {length} bytes (max {MAX_FRAME_SIZE})"),
            ));
        }

        // Check if we have the full frame body yet.
        let total = 4 + length;
        if src.len() < total {
            // Reserve space for the rest of the frame so the next read is efficient.
            src.reserve(total - src.len());
            return Ok(None);
        }

        // Consume the length prefix.
        src.advance(4);

        // Read header fields.
        let seq_no = src.get_u32();
        let ack = src.get_u32();
        let flags = FrameFlags::from_bits(src.get_u8());

        // Read payload.
        let payload_len = length - HEADER_SIZE;
        let payload = Bytes::copy_from_slice(&src[..payload_len]);
        src.advance(payload_len);

        Ok(Some(Frame {
            seq_no,
            ack,
            flags,
            payload,
        }))
    }
}

impl Encoder<Frame> for FrameCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> std::result::Result<(), Self::Error> {
        let payload_len = item.payload.len();
        let body_len = HEADER_SIZE + payload_len;

        if body_len > MAX_FRAME_SIZE as usize {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("frame too large: {body_len} bytes (max {MAX_FRAME_SIZE})"),
            ));
        }

        dst.reserve(4 + body_len);
        dst.put_u32(body_len as u32);
        dst.put_u32(item.seq_no);
        dst.put_u32(item.ack);
        dst.put_u8(item.flags.bits());
        dst.put_slice(&item.payload);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut codec = FrameCodec::new();
        let frame = Frame::new(1, 0, FrameFlags::ENCRYPTED, Bytes::from_static(b"hello"));

        let mut buf = BytesMut::new();
        codec.encode(frame.clone(), &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded.seq_no, 1);
        assert_eq!(decoded.ack, 0);
        assert_eq!(decoded.flags, FrameFlags::ENCRYPTED);
        assert_eq!(decoded.payload, Bytes::from_static(b"hello"));
    }

    #[test]
    fn partial_read() {
        let mut codec = FrameCodec::new();
        let frame = Frame::new(42, 10, FrameFlags::PING, Bytes::from_static(b"ping"));

        let mut buf = BytesMut::new();
        codec.encode(frame, &mut buf).unwrap();

        // Split the buffer to simulate a partial read.
        let rest = buf.split_off(6);
        assert!(codec.decode(&mut buf).unwrap().is_none());

        // Re-combine and decode.
        buf.unsplit(rest);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded.seq_no, 42);
        assert_eq!(decoded.payload, Bytes::from_static(b"ping"));
    }
}
