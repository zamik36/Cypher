use cypher_core::Command;
use cypher_types::{MsgId, PeerId};
use tauri::State;

use crate::session::{AppState, CmdResult, err};

pub(crate) fn parse_peer(hex: &str) -> CmdResult<PeerId> {
    PeerId::from_hex(hex).ok_or_else(|| "invalid peer id".to_owned())
}

fn parse_msg(hex: &str) -> CmdResult<MsgId> {
    MsgId::from_hex(hex).ok_or_else(|| "invalid message id".to_owned())
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
    let id = client.send_text(peer, text, reply_to).await.map_err(err)?;
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
        .map_err(err)
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
    let ids = msg_ids.iter().filter_map(|h| MsgId::from_hex(h)).collect();
    state
        .client()
        .await?
        .command(Command::MarkRead { peer, ids })
        .await
        .map_err(err)
}
