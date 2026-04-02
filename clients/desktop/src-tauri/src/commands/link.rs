use serde::{Deserialize, Serialize};

use crate::{current_api, AppState};

#[derive(Debug, Serialize, Deserialize)]
pub struct LinkInfo {
    pub link_id: String,
}

/// Ask the signaling service to create a new share link.
#[tauri::command]
pub async fn create_link(state: tauri::State<'_, AppState>) -> Result<LinkInfo, String> {
    let api = current_api(&state).await;
    let link_id = api.create_link().await.map_err(|e| e.to_string())?;
    Ok(LinkInfo { link_id })
}

/// Join an existing share link: resolves the remote peer, initiates E2EE, and
/// stores the peer as the current conversation partner.
#[tauri::command]
pub async fn join_link(
    state: tauri::State<'_, AppState>,
    link_id: String,
) -> Result<String, String> {
    let api = current_api(&state).await;
    let peer_id = api.join_link(&link_id).await.map_err(|e| e.to_string())?;

    // Initiate the X3DH session as the joiner (we initiated the connection).
    api.initiate_session(&peer_id)
        .await
        .map_err(|e| e.to_string())?;

    // Add peer to the known peers set (O(1) dedup).
    {
        let mut set = state.peers.lock().await;
        set.insert(peer_id.clone());
    }

    let hex: String = peer_id
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(hex)
}
