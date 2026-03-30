use serde::{Deserialize, Serialize};

use crate::AppState;

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

    let pid = cypher_common::PeerId::from_bytes(&peer_id_bytes)
        .ok_or("peer_id must be 32 bytes")?;

    let api = state.api.lock().await;
    api.send_message(&pid, text.as_bytes())
        .await
        .map_err(|e| e.to_string())
}

/// Return an empty list — messages are delivered via Tauri events, not polled.
#[tauri::command]
pub async fn get_messages() -> Result<Vec<ChatMessage>, String> {
    Ok(Vec::new())
}
