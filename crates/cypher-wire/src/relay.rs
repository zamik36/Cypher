//! Frames on the onion relay: `[corr u64][tag u8][payload]` from the client,
//! `[corr u64][reply]` back. The correlation id is the client's, echoed so
//! pipelined requests can be told apart.

pub const CORR_LEN: usize = 8;
/// What follows the tag.
pub const HEADER_LEN: usize = CORR_LEN + 1;

/// A request sealed to signaling's onion key (`[epoch u32][sealed]`).
pub const TAG_ONION: u8 = 1;
/// Signaling's signed onion key set; no payload.
pub const TAG_KEYS: u8 = 2;
