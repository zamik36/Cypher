//! End-to-end message envelope: everything here is encrypted by the ratchet
//! and invisible to the server.

use cypher_types::{FileId, MsgId};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::CoreError;
use crate::api::MediaKind;

pub const MAX_TEXT_LEN: usize = 16 * 1024;
pub const MAX_INLINE_LEN: usize = 32 * 1024;
pub const MAX_NAME_LEN: usize = 255;
pub const MAX_MIME_LEN: usize = 127;
pub const MAX_WAVEFORM_LEN: usize = 128;
pub const MAX_POSTER_LEN: usize = 16 * 1024;
pub const MAX_RECEIPT_IDS: usize = 512;
pub const MAX_FILE_SIZE: u64 = 64 << 30;
pub const MAX_CHUNK_SIZE: u32 = 1 << 20;
pub const FILE_CHUNK_SIZE: u32 = 256 * 1024;
pub const MEDIA_CHUNK_SIZE: u32 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub msg_id: MsgId,
    pub sent_at_ms: u64,
    pub body: Body,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Body {
    Hello {
        inbox: [u8; 32],
    },
    Text {
        text: String,
        reply_to: Option<MsgId>,
    },
    File {
        desc: FileDesc,
        kind: MediaKind,
    },
    FileCtl(FileCtl),
    Receipt {
        kind: ReceiptKind,
        ids: Vec<MsgId>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReceiptKind {
    Delivered,
    Read,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileCtl {
    /// Start or resume: `have` is a bitmap of chunks already stored.
    Accept {
        file_id: FileId,
        have: Vec<u8>,
    },
    Cancel {
        file_id: FileId,
    },
}

/// Describes a transfer. The key never leaves the encrypted envelope.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDesc {
    pub file_id: FileId,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub chunk_size: u32,
    pub key: [u8; 32],
    pub inline: Option<Vec<u8>>,
}

impl Drop for FileDesc {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl std::fmt::Debug for FileDesc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileDesc")
            .field("file_id", &self.file_id)
            .field("size", &self.size)
            .field("chunk_size", &self.chunk_size)
            .finish_non_exhaustive()
    }
}

impl FileDesc {
    pub fn chunk_count(&self) -> u32 {
        chunk_count(self.size, self.chunk_size)
    }

    /// Plaintext length of chunk `index`.
    pub fn chunk_len(&self, index: u32) -> u32 {
        let start = u64::from(index) * u64::from(self.chunk_size);
        u32::try_from(
            self.size
                .saturating_sub(start)
                .min(u64::from(self.chunk_size)),
        )
        .expect("bounded by chunk_size")
    }

    fn validate(&self) -> Result<(), CoreError> {
        let ok = self.name.len() <= MAX_NAME_LEN
            && self.mime.len() <= MAX_MIME_LEN
            && self.size <= MAX_FILE_SIZE
            && (1..=MAX_CHUNK_SIZE).contains(&self.chunk_size)
            && u64::from(self.chunk_count()) * u64::from(self.chunk_size) >= self.size
            && self.inline.as_ref().is_none_or(|b| {
                b.len() <= MAX_INLINE_LEN && b.len() as u64 == self.size && self.chunk_count() == 1
            });
        if ok { Ok(()) } else { Err(CoreError::Invalid) }
    }
}

pub fn chunk_count(size: u64, chunk_size: u32) -> u32 {
    u32::try_from(size.div_ceil(u64::from(chunk_size)).max(1)).unwrap_or(u32::MAX)
}

impl Envelope {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = postcard::to_allocvec(self).expect("in-memory serialization");
        buf.resize(padded_len(buf.len()), 0);
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, CoreError> {
        let (env, _padding) =
            postcard::take_from_bytes::<Self>(buf).map_err(|_| CoreError::Invalid)?;
        env.validate()?;
        Ok(env)
    }

    fn validate(&self) -> Result<(), CoreError> {
        let ok = match &self.body {
            Body::Hello { .. } | Body::FileCtl(FileCtl::Cancel { .. }) => true,
            Body::Text { text, .. } => text.len() <= MAX_TEXT_LEN,
            Body::File { desc, kind } => {
                desc.validate()?;
                match kind {
                    MediaKind::File => true,
                    MediaKind::Voice { waveform, .. } => waveform.len() <= MAX_WAVEFORM_LEN,
                    MediaKind::VideoNote { poster, .. } => poster.len() <= MAX_POSTER_LEN,
                }
            }
            Body::FileCtl(FileCtl::Accept { have, .. }) => {
                have.len() <= (u32::MAX as usize).div_ceil(8)
            }
            Body::Receipt { ids, .. } => ids.len() <= MAX_RECEIPT_IDS,
        };
        if ok { Ok(()) } else { Err(CoreError::Invalid) }
    }
}

/// Hides the exact plaintext length: power-of-two buckets from 256 B to
/// 64 KiB, then 64 KiB granularity.
pub fn padded_len(n: usize) -> usize {
    const MIN: usize = 256;
    const STEP: usize = 64 * 1024;
    if n <= STEP {
        n.max(MIN).next_power_of_two()
    } else {
        n.div_ceil(STEP) * STEP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(size: u64, chunk_size: u32) -> FileDesc {
        FileDesc {
            file_id: FileId([1; 16]),
            name: "a.txt".into(),
            mime: "text/plain".into(),
            size,
            chunk_size,
            key: [2; 32],
            inline: None,
        }
    }

    #[test]
    fn padding_buckets() {
        assert_eq!(padded_len(0), 256);
        assert_eq!(padded_len(256), 256);
        assert_eq!(padded_len(257), 512);
        assert_eq!(padded_len(65_536), 65_536);
        assert_eq!(padded_len(65_537), 131_072);
    }

    #[test]
    fn envelope_roundtrip_with_padding() {
        let env = Envelope {
            msg_id: MsgId([3; 16]),
            sent_at_ms: 1_700_000_000_000,
            body: Body::Text {
                text: "{\"json-like\":1}".into(),
                reply_to: None,
            },
        };
        let buf = env.encode();
        assert_eq!(buf.len(), 256);
        assert_eq!(Envelope::decode(&buf).unwrap(), env);
    }

    #[test]
    fn chunk_math() {
        let d = desc(10, 4);
        assert_eq!(d.chunk_count(), 3);
        assert_eq!((d.chunk_len(0), d.chunk_len(2)), (4, 2));
        assert_eq!(desc(0, 4).chunk_count(), 1);
        assert_eq!(desc(0, 4).chunk_len(0), 0);
    }

    #[test]
    fn hostile_descriptors_are_rejected() {
        let wrap = |desc: FileDesc| Envelope {
            msg_id: MsgId([0; 16]),
            sent_at_ms: 0,
            body: Body::File {
                desc,
                kind: MediaKind::File,
            },
        };
        assert!(Envelope::decode(&wrap(desc(MAX_FILE_SIZE + 1, 4)).encode()).is_err());
        assert!(Envelope::decode(&wrap(desc(10, 0)).encode()).is_err());
        assert!(Envelope::decode(&wrap(desc(10, MAX_CHUNK_SIZE + 1)).encode()).is_err());
        let mut lying_inline = desc(10, 10);
        lying_inline.inline = Some(vec![0; 3]);
        assert!(Envelope::decode(&wrap(lying_inline).encode()).is_err());
        let mut long_name = desc(10, 4);
        long_name.name = "x".repeat(MAX_NAME_LEN + 1);
        assert!(Envelope::decode(&wrap(long_name).encode()).is_err());
        assert!(Envelope::decode(&[0xFF; 8]).is_err());
    }
}
