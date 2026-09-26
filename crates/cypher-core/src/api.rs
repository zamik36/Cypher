use bytes::Bytes;
use cypher_types::{FileId, MsgId, PeerId};
use serde::{Deserialize, Serialize};

use crate::store::StoreOp;

/// Everything the driver feeds into [`crate::Core::handle`].
#[derive(Debug)]
pub enum Input {
    /// Transport to the gateway is up; the core starts authentication.
    Connected,
    Disconnected,
    /// A frame from the gateway session.
    Frame(Bytes),
    /// A reply from the anonymous channel, as produced by the relay.
    AnonymousFrame(Bytes),
    /// The driver's relay (direct TLS or via Tor) went up or down.
    AnonymousChannel {
        up: bool,
    },
    Command(Command),
    /// Answer to [`Effect::ReadChunk`]: `buf[..headroom]` is reserved for the
    /// core to write frame headers in place, the rest holds the chunk.
    ChunkRead {
        file_id: FileId,
        index: u32,
        buf: Vec<u8>,
    },
    /// Answer to [`Effect::ReadChunk`] when the source is gone.
    ChunkUnavailable {
        file_id: FileId,
    },
    /// Periodic timer, at least once per second while running.
    Tick,
}

#[derive(Debug, Clone)]
pub enum Command {
    CreateLink,
    JoinLink {
        link: String,
    },
    SendText {
        peer: PeerId,
        msg_id: MsgId,
        text: String,
        reply_to: Option<MsgId>,
    },
    /// Offers a file or media message. `inline` carries the full content for
    /// small media, which then travels inside the encrypted message itself.
    SendFile {
        peer: PeerId,
        msg_id: MsgId,
        file_id: FileId,
        name: String,
        mime: String,
        size: u64,
        kind: MediaKind,
        inline: Option<Vec<u8>>,
    },
    AcceptFile {
        file_id: FileId,
    },
    CancelTransfer {
        file_id: FileId,
    },
    MarkRead {
        peer: PeerId,
        ids: Vec<MsgId>,
    },
    FetchInbox,
    RemovePeer {
        peer: PeerId,
    },
    /// With `require_onion`, inbox traffic never falls back to the
    /// identity-bearing gateway session.
    SetAnonymity {
        require_onion: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    File,
    Voice { duration_ms: u32, waveform: Vec<u8> },
    VideoNote { duration_ms: u32, poster: Vec<u8> },
}

impl MediaKind {
    pub fn is_media(&self) -> bool {
        !matches!(self, Self::File)
    }
}

/// Side effects the driver must perform, strictly in order: a `Persist` must
/// be durable before any later `Transmit` leaves the process.
#[derive(Debug)]
pub enum Effect {
    Transmit(Bytes),
    /// Onion-sealed request for the relay channel (`[corr u64][blob]`).
    Anonymous(Bytes),
    Persist(StoreOp),
    Emit(Event),
    ReadChunk {
        file_id: FileId,
        index: u32,
        offset: u64,
        len: u32,
        headroom: usize,
    },
    /// Allocate `len` bytes for a transfer. `sealed` media is kept as the
    /// received ciphertext chunks (each `chunk_size + 16` bytes) and is only
    /// decrypted on playback.
    OpenSink {
        file_id: FileId,
        len: u64,
        sealed: bool,
    },
    WriteChunk {
        file_id: FileId,
        offset: u64,
        data: Bytes,
    },
    CloseSink {
        file_id: FileId,
        complete: bool,
    },
    /// Drop the gateway connection (protocol violation, liveness timeout).
    Disconnect {
        reconnect: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Connected,
    Disconnected,
    Superseded,
    Bootstrap {
        relay_addr: String,
        onion_key: [u8; 32],
    },
    LinkCreated {
        link: String,
    },
    JoinFailed {
        link: String,
        reason: FailReason,
    },
    PeerAdded {
        peer: PeerId,
        initiated_by_us: bool,
    },
    Message(StoredMessage),
    MessageStatus {
        msg_id: MsgId,
        status: MessageStatus,
    },
    TransferOffered {
        peer: PeerId,
        file_id: FileId,
        name: String,
        size: u64,
        mime: String,
    },
    TransferProgress {
        file_id: FileId,
        bytes: u64,
        total: u64,
    },
    TransferComplete {
        file_id: FileId,
    },
    TransferFailed {
        file_id: FileId,
        reason: FailReason,
    },
    Warning {
        reason: FailReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailReason {
    NotFound,
    InvalidLink,
    SelfLink,
    InvalidKeys,
    Timeout,
    Offline,
    Rejected,
    Cancelled,
    SourceUnavailable,
    Corrupted,
    Unauthorized,
    ServerError,
    DecryptFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Pending,
    Sent,
    Queued,
    Delivered,
    Read,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMessage {
    pub msg_id: MsgId,
    pub peer: PeerId,
    pub outgoing: bool,
    pub sent_at_ms: u64,
    pub status: MessageStatus,
    pub content: Content,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
        reply_to: Option<MsgId>,
    },
    File {
        file_id: FileId,
        name: String,
        mime: String,
        size: u64,
        kind: MediaKind,
    },
}
