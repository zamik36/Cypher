use cypher_client::{IdentityStore, Unlocked};
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::dto::{self, UiMessage};
use crate::session::{AppState, CmdResult, data_dir, err};

/// Argon2id is deliberately slow; keep it off the async runtime.
async fn with_store<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce(IdentityStore) -> Result<T, cypher_client::ClientError> + Send + 'static,
) -> CmdResult<T> {
    let store = IdentityStore::new(&data_dir(app)?);
    tokio::task::spawn_blocking(move || f(store))
        .await
        .map_err(err)?
        .map_err(err)
}

async fn activate(state: &AppState, unlocked: Unlocked) -> String {
    let peer = unlocked.seed.derive_identity().peer_id().to_hex();
    state.set_identity(unlocked.seed, unlocked.nickname).await;
    peer
}

#[tauri::command]
pub async fn has_identity(app: AppHandle) -> CmdResult<bool> {
    Ok(IdentityStore::new(&data_dir(&app)?).exists())
}

#[tauri::command]
pub async fn create_identity(
    app: AppHandle,
    state: State<'_, AppState>,
    nickname: String,
    passphrase: String,
) -> CmdResult<String> {
    let unlocked = with_store(&app, move |s| s.create(&nickname, &passphrase)).await?;
    Ok(activate(&state, unlocked).await)
}

#[tauri::command]
pub async fn unlock_identity(
    app: AppHandle,
    state: State<'_, AppState>,
    passphrase: String,
) -> CmdResult<(String, String)> {
    let unlocked = with_store(&app, move |s| s.unlock(&passphrase)).await?;
    let nickname = unlocked.nickname.clone();
    Ok((activate(&state, unlocked).await, nickname))
}

#[tauri::command]
pub async fn import_mnemonic(
    app: AppHandle,
    state: State<'_, AppState>,
    mnemonic: String,
    nickname: String,
    passphrase: String,
) -> CmdResult<String> {
    let unlocked = with_store(&app, move |s| s.import(&mnemonic, &nickname, &passphrase)).await?;
    Ok(activate(&state, unlocked).await)
}

/// Re-verifies the passphrase before revealing the recovery phrase.
#[tauri::command]
pub async fn export_mnemonic(app: AppHandle, passphrase: String) -> CmdResult<String> {
    with_store(&app, move |s| {
        s.unlock(&passphrase).map(|u| u.seed.to_mnemonic())
    })
    .await
}

#[derive(Serialize)]
pub struct Conversation {
    peer_id: String,
    display_name: Option<String>,
    last_message_at: u64,
}

#[tauri::command]
pub async fn get_conversations(state: State<'_, AppState>) -> CmdResult<Vec<Conversation>> {
    let client = state.client().await?;
    let mut out = Vec::new();
    for peer in client.contacts().await.map_err(err)? {
        let last = client.history(peer, None, 1).await.map_err(err)?;
        out.push(Conversation {
            peer_id: peer.to_hex(),
            display_name: None,
            last_message_at: last.first().map_or(0, |m| m.sent_at_ms),
        });
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.last_message_at));
    Ok(out)
}

#[tauri::command]
pub async fn get_history(
    state: State<'_, AppState>,
    peer_id: String,
    before: Option<u64>,
    limit: usize,
) -> CmdResult<Vec<UiMessage>> {
    let peer = cypher_types::PeerId::from_hex(&peer_id).ok_or("invalid peer id")?;
    let client = state.client().await?;
    let history = client
        .history(peer, before, limit.min(500))
        .await
        .map_err(err)?;
    Ok(history.iter().map(dto::message).collect())
}

#[tauri::command]
pub async fn clear_chat_history(state: State<'_, AppState>) -> CmdResult<()> {
    state.client().await?.clear_history().await.map_err(err)
}
