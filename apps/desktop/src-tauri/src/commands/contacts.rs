//! What the user decides about a contact: accepting, blocking, naming.

use cypher_core::Command;
use tauri::State;

use super::chat::parse_peer;
use crate::session::{AppState, CmdResult, err};

/// Takes someone who wrote without an invite as a contact.
#[tauri::command]
pub(crate) async fn accept_contact(state: State<'_, AppState>, peer_id: String) -> CmdResult<()> {
    let peer = parse_peer(&peer_id)?;
    contact_command(&state, Command::AcceptContact { peer }).await
}

/// Blocks or unblocks a contact.
#[tauri::command]
pub(crate) async fn set_blocked(
    state: State<'_, AppState>,
    peer_id: String,
    blocked: bool,
) -> CmdResult<()> {
    let peer = parse_peer(&peer_id)?;
    let cmd = if blocked {
        Command::BlockPeer { peer }
    } else {
        Command::UnblockPeer { peer }
    };
    contact_command(&state, cmd).await
}

async fn contact_command(state: &AppState, cmd: Command) -> CmdResult<()> {
    state.client().await?.command(cmd).await.map_err(err)
}

/// Names a contact on this device; an empty name removes it.
#[tauri::command]
pub(crate) async fn rename_peer(
    state: State<'_, AppState>,
    peer_id: String,
    alias: Option<String>,
) -> CmdResult<()> {
    let peer = parse_peer(&peer_id)?;
    state
        .client()
        .await?
        .rename_contact(peer, alias)
        .await
        .map_err(err)
}
