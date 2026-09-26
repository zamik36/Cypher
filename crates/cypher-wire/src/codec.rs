use bytes::{BufMut, Bytes, BytesMut};

use crate::WireError;

/// Zero-copy cursor over a received frame: variable-length fields are
/// returned as cheap `Bytes` slices of the original buffer.
pub(crate) struct Reader {
    buf: Bytes,
}

impl Reader {
    pub(crate) fn new(buf: Bytes) -> Self {
        Self { buf }
    }

    fn need(&self, n: usize) -> Result<(), WireError> {
        if self.buf.len() < n {
            Err(WireError::Truncated)
        } else {
            Ok(())
        }
    }

    pub(crate) fn u8(&mut self) -> Result<u8, WireError> {
        self.need(1)?;
        let v = self.buf[0];
        self.buf = self.buf.slice(1..);
        Ok(v)
    }

    pub(crate) fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        self.need(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(&self.buf[..N]);
        self.buf = self.buf.slice(N..);
        Ok(out)
    }

    /// `u32` length-prefixed byte string, bounded by `max`.
    pub(crate) fn bytes(&mut self, max: usize) -> Result<Bytes, WireError> {
        let len = self.u32()? as usize;
        if len > max {
            return Err(WireError::TooLarge);
        }
        self.need(len)?;
        Ok(self.buf.split_to(len))
    }

    /// `u8` length-prefixed UTF-8 string.
    pub(crate) fn short_str(&mut self) -> Result<String, WireError> {
        let len = self.u8()? as usize;
        self.need(len)?;
        let raw = self.buf.split_to(len);
        String::from_utf8(raw.to_vec()).map_err(|_| WireError::Malformed)
    }

    /// All remaining bytes; the field must be last in the message.
    pub(crate) fn rest(self) -> Bytes {
        self.buf
    }

    pub(crate) fn finish(self) -> Result<(), WireError> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(WireError::TrailingBytes)
        }
    }
}

pub(crate) trait WriteExt {
    fn put_bytes_prefixed(&mut self, b: &[u8]);
    fn put_short_str(&mut self, s: &str);
}

impl WriteExt for BytesMut {
    fn put_bytes_prefixed(&mut self, b: &[u8]) {
        self.put_u32_le(u32::try_from(b.len()).expect("field bounded by frame size"));
        self.put_slice(b);
    }

    fn put_short_str(&mut self, s: &str) {
        let len = u8::try_from(s.len()).expect("short string bounded to 255 bytes");
        self.put_u8(len);
        self.put_slice(s.as_bytes());
    }
}
