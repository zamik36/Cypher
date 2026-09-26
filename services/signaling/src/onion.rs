use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use cypher_crypto::onion;
use cypher_wire::{ClientMsg, Frame};
use x25519_dalek::StaticSecret;

use crate::handler::Handler;

/// Requests outside this window are treated as replays.
const MAX_SKEW_SECS: u64 = 120;

/// Opens an onion-sealed request, answers it anonymously and seals the reply.
/// Returns `None` for anything that must be dropped silently.
pub async fn handle(handler: &Handler, secret: &StaticSecret, blob: &[u8]) -> Option<Bytes> {
    let opened = onion::open_request(secret, blob).ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    if now.abs_diff(opened.timestamp_secs) > MAX_SKEW_SECS {
        return None;
    }
    let frame = Bytes::from(opened.frame);
    let allowed = matches!(
        Frame::<ClientMsg>::decode(frame.clone()).ok()?.msg,
        ClientMsg::InboxPut { .. } | ClientMsg::InboxFetch { .. } | ClientMsg::InboxAck { .. }
    );
    if !allowed {
        return None;
    }
    let reply = handler.handle(None, frame).await;
    Some(Bytes::from(onion::seal_response(&opened.reply, &reply)))
}
