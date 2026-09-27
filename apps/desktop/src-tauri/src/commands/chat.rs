use cypher_core::Command;
use cypher_types::{MsgId, PeerId};
use tauri::State;

use crate::session::{AppState, CmdResult, err};

pub(crate) fn parse_peer(hex: &str) -> CmdResult<PeerId> {
    PeerId::from_hex(hex).ok_or_else(|| "invalid peer id".to_owned())
}

/// Returns the message id so the UI can track delivery status.
#[tauri::command]
pub(crate) async fn send_message(
    state: State<'_, AppState>,
    peer_id: String,
    text: String,
) -> CmdResult<String> {
    let peer = parse_peer(&peer_id)?;
    let client = state.client().await?;
    let id = client.send_text(peer, text).await.map_err(err)?;
    Ok(id.to_hex())
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
