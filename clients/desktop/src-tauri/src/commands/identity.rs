use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::Manager;

use crate::{current_api, restart_event_loop, AppState};
use cypher_client_core::identity_store::IdentityStore;
use cypher_client_core::persistence::sqlite::SqliteMessageStore;
use cypher_client_core::persistence::MessageStore;
use cypher_client_core::ClientApi;
use cypher_crypto::IdentitySeed;

// ---------------------------------------------------------------------------
// Identity management
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn has_identity(app: tauri::AppHandle) -> Result<bool, String> {
    Ok(IdentityStore::new(data_dir(&app)?).has_identity())
}

#[tauri::command]
pub async fn create_identity(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    nickname: String,
    passphrase: String,
) -> Result<String, String> {
    let dir = data_dir(&app)?;
    let seed = IdentityStore::new(&dir)
        .create(&nickname, &passphrase)
        .map_err(|e| e.to_string())?;
    let peer_id = activate_identity(&app, &state, &seed, &dir).await?;
    *state.nickname.lock().await = Some(nickname);
    Ok(peer_id)
}

#[tauri::command]
pub async fn unlock_identity(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    passphrase: String,
) -> Result<(String, String), String> {
    let dir = data_dir(&app)?;
    let (seed, nickname) = IdentityStore::new(&dir)
        .unlock(&passphrase)
        .map_err(|e| e.to_string())?;
    let peer_id = activate_identity(&app, &state, &seed, &dir).await?;
    *state.nickname.lock().await = Some(nickname.clone());
    Ok((peer_id, nickname))
}

#[tauri::command]
pub async fn export_mnemonic(app: tauri::AppHandle, passphrase: String) -> Result<String, String> {
    IdentityStore::new(data_dir(&app)?)
        .export_mnemonic(&passphrase)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn import_mnemonic(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    mnemonic: String,
    nickname: String,
    passphrase: String,
) -> Result<String, String> {
    let dir = data_dir(&app)?;
    let seed = IdentityStore::new(&dir)
        .import_mnemonic(&mnemonic, &nickname, &passphrase)
        .map_err(|e| e.to_string())?;
    let peer_id = activate_identity(&app, &state, &seed, &dir).await?;
    *state.nickname.lock().await = Some(nickname);
    Ok(peer_id)
}

// ---------------------------------------------------------------------------
// Chat history
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_conversations(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let api = current_api(&state).await;
    let Some(store) = api.message_store() else {
        return Ok(Vec::new());
    };
    store
        .list_conversations()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|c| {
            Ok(serde_json::json!({
                "peer_id": hex_encode(&c.peer_id),
                "nickname": c.nickname,
                "created_at": c.created_at,
                "last_message_at": c.last_message_at,
            }))
        })
        .collect()
}

#[tauri::command]
pub async fn get_history(
    state: tauri::State<'_, AppState>,
    peer_id: String,
    limit: u32,
    before_id: Option<u64>,
) -> Result<Vec<serde_json::Value>, String> {
    let api = current_api(&state).await;
    let Some(store) = api.message_store() else {
        return Ok(Vec::new());
    };
    let pid = parse_peer_id(&peer_id)?;
    store
        .load_messages(&pid, limit, before_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|m| {
            Ok(serde_json::json!({
                "id": m.id,
                "peer_id": hex_encode(&m.peer_id),
                "direction": match m.direction {
                    cypher_client_core::persistence::Direction::Sent => "sent",
                    cypher_client_core::persistence::Direction::Received => "received",
                },
                "text": String::from_utf8_lossy(&m.plaintext),
                "timestamp": m.timestamp,
            }))
        })
        .collect()
}

#[tauri::command]
pub async fn clear_chat_history(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let api = current_api(&state).await;
    if let Some(store) = api.message_store() {
        store.clear_all().map_err(|e| e.to_string())?;
    }
    api.keys().clear_sessions();
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Initialize a ClientApi from a seed + open the message DB, replace in AppState.
async fn activate_identity(
    app: &tauri::AppHandle,
    state: &AppState,
    seed: &IdentitySeed,
    data_dir: &Path,
) -> Result<String, String> {
    let sek = seed.derive_storage_key();
    let msg_store: Arc<dyn MessageStore> = Arc::new(
        SqliteMessageStore::open(data_dir.join("messages.db"), sek).map_err(|e| e.to_string())?,
    );

    let api = Arc::new(ClientApi::with_seed(seed, Some(msg_store.clone())));

    // Restore ratchet states for known conversations.
    if let Ok(convos) = msg_store.list_conversations() {
        for conv in convos {
            if let Some(pid) = cypher_common::PeerId::from_bytes(&conv.peer_id) {
                if let Ok(Some(ratchet)) = msg_store.load_ratchet_state(&pid) {
                    api.keys().restore_ratchet_state(pid.as_bytes(), ratchet);
                }
            }
        }
    }

    let peer_id = hex_encode(api.peer_id().as_bytes());
    {
        let mut current = state.api.write().await;
        *current = Arc::clone(&api);
    }
    state.peers.lock().await.clear();
    restart_event_loop(state, app.clone()).await;
    Ok(peer_id)
}

fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|e| format!("no data dir: {e}"))
}

fn parse_peer_id(hex: &str) -> Result<cypher_common::PeerId, String> {
    let bytes = hex_decode(hex)?;
    cypher_common::PeerId::from_bytes(&bytes).ok_or("peer_id must be 32 bytes".into())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err("odd-length hex string".into());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| format!("invalid hex: {e}")))
        .collect()
}
