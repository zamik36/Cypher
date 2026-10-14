use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use cypher_crypto::onion;
use cypher_wire::{ClientMsg, Frame};
use rand::rngs::OsRng;

use crate::handler::Handler;
use crate::keys::OnionKeys;

/// Requests outside this window are rejected; inside it, the store
/// remembers every request so a replay is answered at most once.
pub(crate) const MAX_SKEW_SECS: u64 = 120;

/// Opens an onion-sealed request with the key of the epoch it names,
/// answers it anonymously and seals the reply. Returns `None` for anything
/// that must be dropped silently: replays, and keys this server no longer
/// (or never) had.
pub(crate) async fn handle(handler: &Handler, keys: &OnionKeys, blob: &[u8]) -> Option<Bytes> {
    let (epoch, sealed) = onion::split_epoch(blob).ok()?;
    let opened = onion::open_request(&keys.secret(epoch)?, sealed).ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    if now.abs_diff(opened.timestamp_secs) > MAX_SKEW_SECS
        || !handler.store.first_seen(&opened.replay_id).await.ok()?
    {
        return None;
    }
    let frame = Bytes::from(opened.frame);
    let allowed = matches!(
        Frame::<ClientMsg>::decode(frame.clone()).ok()?.msg,
        ClientMsg::InboxPut { .. }
            | ClientMsg::InboxFetch { .. }
            | ClientMsg::InboxAck { .. }
            | ClientMsg::PushKey
            | ClientMsg::PushRegister { .. }
            | ClientMsg::PushUnregister { .. }
            | ClientMsg::FetchKeys { .. }
            | ClientMsg::FetchDevices { .. }
            | ClientMsg::ResolveLink { .. }
    );
    if !allowed {
        return None;
    }
    let reply = handler.handle(None, frame).await;
    Some(Bytes::from(onion::seal_response(
        &opened.reply,
        &reply,
        &mut OsRng,
    )))
}
