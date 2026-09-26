use cypher_core::Command;
use tauri::{AppHandle, State};

use crate::session::{AppState, CmdResult, err};

/// Starts (or restarts) the client against `addr` and returns our peer id.
#[tauri::command]
pub async fn connect_to_gateway(
    app: AppHandle,
    state: State<'_, AppState>,
    addr: String,
    require_onion: bool,
) -> CmdResult<String> {
    let addr = addr.trim();
    cypher_transport::split_host_port(addr).map_err(err)?;
    state.connect(&app, addr.to_owned(), require_onion).await
}

#[tauri::command]
pub async fn apply_anonymous_settings(
    state: State<'_, AppState>,
    require_onion: bool,
) -> CmdResult<()> {
    state
        .client()
        .await?
        .command(Command::SetAnonymity { require_onion })
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn get_nickname(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    Ok(state.nickname().await)
}
