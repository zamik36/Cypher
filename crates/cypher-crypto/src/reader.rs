use crate::error::CryptoError;

/// Bounds-checked little-endian cursor over untrusted bytes.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], CryptoError> {
        if self.buf.len() < n {
            return Err(CryptoError::Malformed);
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], CryptoError> {
        self.take(N)?.try_into().map_err(|_| CryptoError::Malformed)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, CryptoError> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u32(&mut self) -> Result<u32, CryptoError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, CryptoError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    pub(crate) fn rest(self) -> &'a [u8] {
        self.buf
    }

    pub(crate) fn finish(self) -> Result<(), CryptoError> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(CryptoError::Malformed)
        }
    }
}
