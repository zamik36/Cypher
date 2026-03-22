use bytes::Bytes;

/// Flags for a transport frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameFlags(u8);

impl FrameFlags {
    pub const NONE: Self = Self(0x00);
    pub const ACK_ONLY: Self = Self(0x01);
    pub const ENCRYPTED: Self = Self(0x02);
    pub const COMPRESSED: Self = Self(0x04);
    pub const PING: Self = Self(0x08);
    pub const PONG: Self = Self(0x10);
    pub const SESSION_INIT: Self = Self(0x20);
    pub const SESSION_CLOSE: Self = Self(0x40);

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for FrameFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::ops::BitAnd for FrameFlags {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        self.intersection(rhs)
    }
}

/// A single transport frame on the wire.
#[derive(Debug, Clone)]
pub struct Frame {
    pub seq_no: u32,
    pub ack: u32,
    pub flags: FrameFlags,
    pub payload: Bytes,
}

impl Frame {
    pub fn new(seq_no: u32, ack: u32, flags: FrameFlags, payload: Bytes) -> Self {
        Self {
            seq_no,
            ack,
            flags,
            payload,
        }
    }
}
