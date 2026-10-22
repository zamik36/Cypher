use cypher_core::{CmdError, Command, ErrorKind};
use cypher_types::{MsgId, PeerId};
use tauri::State;

use crate::session::{AppState, CmdResult};

pub(crate) fn parse_peer(hex: &str) -> CmdResult<PeerId> {
    PeerId::from_hex(hex).ok_or_else(|| CmdError::new(ErrorKind::Invalid, "invalid peer id"))
}

fn parse_msg(hex: &str) -> CmdResult<MsgId> {
    MsgId::from_hex(hex).ok_or_else(|| CmdError::new(ErrorKind::Invalid, "invalid message id"))
}

/// Returns the message id so the UI can track delivery status.
#[tauri::command]
pub(crate) async fn send_message(
    state: State<'_, AppState>,
    peer_id: String,
    text: String,
    reply_to: Option<String>,
) -> CmdResult<String> {
    let peer = parse_peer(&peer_id)?;
    let reply_to = reply_to.as_deref().map(parse_msg).transpose()?;
    let client = state.client().await?;
    let id = client
        .send_text(peer, text, reply_to)
        .await
        .map_err(CmdError::from)?;
    Ok(id.to_hex())
}

/// Deletes a message from this device only.
#[tauri::command]
pub(crate) async fn delete_message(
    state: State<'_, AppState>,
    peer_id: String,
    msg_id: String,
    timestamp: u64,
) -> CmdResult<()> {
    let (peer, msg) = (parse_peer(&peer_id)?, parse_msg(&msg_id)?);
    state
        .client()
        .await?
        .delete_message(peer, msg, timestamp)
        .await
        .map_err(CmdError::from)
}

/// The number to compare with `peer_id` out of band (60 digits).
#[tauri::command]
pub(crate) async fn safety_number(
    state: State<'_, AppState>,
    peer_id: String,
) -> CmdResult<String> {
    state.safety_number(&parse_peer(&peer_id)?).await
}

#[tauri::command]
pub(crate) async fn mark_read(
    state: State<'_, AppState>,
    peer_id: String,
    msg_ids: Vec<String>,
) -> CmdResult<()> {
    let peer = parse_peer(&peer_id)?;
    // One malformed id refuses the whole list, before anything is sent.
    let ids = msg_ids
        .iter()
        .map(|h| parse_msg(h))
        .collect::<CmdResult<_>>()?;
    state
        .client()
        .await?
        .command(Command::MarkRead { peer, ids })
        .await
        .map_err(CmdError::from)
}
