use serde::{Deserialize, Serialize};

use crate::{current_api, AppState};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub from: String,
    pub text: String,
    pub timestamp: u64,
}

/// Encrypt and send a chat message to the specified peer.
#[tauri::command]
pub async fn send_message(
    state: tauri::State<'_, AppState>,
    peer_id: String,
    text: String,
) -> Result<(), String> {
    if peer_id.len() != 64 {
        return Err("peer_id must be exactly 64 hex characters (32 bytes)".to_string());
    }

    let peer_id_bytes: Vec<u8> = (0..peer_id.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&peer_id[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|e| format!("invalid peer_id hex: {e}"))?;

    let pid =
        cypher_common::PeerId::from_bytes(&peer_id_bytes).ok_or("peer_id must be 32 bytes")?;

    let api = current_api(&state).await;
    let is_online = state.peers.lock().await.contains(&pid);

    if is_online {
        if !api.keys().has_session(pid.as_bytes()) {
            api.initiate_session(&pid)
                .await
                .map_err(|e| e.to_string())?;
        }

        return api
            .send_message(&pid, text.as_bytes())
            .await
            .map_err(|e| e.to_string());
    }

    let store = api
        .message_store()
        .ok_or_else(|| "message store unavailable".to_string())?;
    let inbox_id = store
        .load_peer_inbox_id(&pid)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            "Offline delivery is unavailable for this chat until the peer reconnects and completes a new key exchange.".to_string()
        })?;

    if !api.keys().has_session(pid.as_bytes()) {
        return Err(
            "Offline delivery is unavailable because this chat has no restored secure session yet."
                .to_string(),
        );
    }

    api.send_message_offline(&pid, &inbox_id, text.as_bytes())
        .await
        .map_err(|e| e.to_string())
}

/// Return an empty list — messages are delivered via Tauri events, not polled.
#[tauri::command]
pub async fn get_messages() -> Result<Vec<ChatMessage>, String> {
    Ok(Vec::new())
}
