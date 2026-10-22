//! The stored conversations: the chat list, a chat's history, deleting.

use cypher_core::CmdError;
use cypher_core::ui::{self, UiConversation, UiMessage};
use tauri::State;

use super::chat::parse_peer;
use crate::session::{AppState, CmdResult};

/// Every conversation, most recent first, as the chat list shows it.
#[tauri::command]
pub(crate) async fn get_conversations(
    state: State<'_, AppState>,
) -> CmdResult<Vec<UiConversation>> {
    state
        .client()
        .await?
        .conversations()
        .await
        .map_err(CmdError::from)
}

/// Ends the session with a contact and deletes the conversation.
#[tauri::command]
pub(crate) async fn delete_conversation(
    state: State<'_, AppState>,
    peer_id: String,
) -> CmdResult<()> {
    let peer = parse_peer(&peer_id)?;
    state
        .client()
        .await?
        .forget_peer(peer)
        .await
        .map_err(CmdError::from)
}

#[tauri::command]
pub(crate) async fn get_history(
    state: State<'_, AppState>,
    peer_id: String,
    before: Option<u64>,
    limit: usize,
) -> CmdResult<Vec<UiMessage>> {
    let peer = parse_peer(&peer_id)?;
    let client = state.client().await?;
    let history = client
        .history(peer, before, limit.min(500))
        .await
        .map_err(CmdError::from)?;
    Ok(history.iter().map(ui::message).collect())
}

#[tauri::command]
pub(crate) async fn clear_chat_history(state: State<'_, AppState>) -> CmdResult<()> {
    state
        .client()
        .await?
        .clear_history()
        .await
        .map_err(CmdError::from)
}
