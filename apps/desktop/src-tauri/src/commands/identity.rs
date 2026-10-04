use cypher_client::{IdentityStore, Unlocked};
use serde::Serialize;
use tauri::State;

use super::chat::parse_peer;
use crate::session::{AppState, CmdResult, err};
use cypher_core::MessageStatus;
use cypher_core::ui::{self, UiMessage};

/// Argon2id is deliberately slow; keep it off the async runtime.
async fn with_store<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(IdentityStore) -> Result<T, cypher_client::ClientError> + Send + 'static,
) -> CmdResult<T> {
    let store = IdentityStore::new(&state.paths().data);
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
pub(crate) async fn has_identity(state: State<'_, AppState>) -> CmdResult<bool> {
    Ok(IdentityStore::new(&state.paths().data).exists())
}

#[tauri::command]
pub(crate) async fn create_identity(
    state: State<'_, AppState>,
    nickname: String,
    passphrase: String,
) -> CmdResult<String> {
    let unlocked = with_store(&state, move |s| s.create(&nickname, &passphrase)).await?;
    Ok(activate(&state, unlocked).await)
}

#[tauri::command]
pub(crate) async fn unlock_identity(
    state: State<'_, AppState>,
    passphrase: String,
) -> CmdResult<(String, String)> {
    let unlocked = with_store(&state, move |s| s.unlock(&passphrase)).await?;
    let nickname = unlocked.nickname.clone();
    Ok((activate(&state, unlocked).await, nickname))
}

#[tauri::command]
pub(crate) async fn import_mnemonic(
    state: State<'_, AppState>,
    mnemonic: String,
    nickname: String,
    passphrase: String,
) -> CmdResult<String> {
    let unlocked = with_store(&state, move |s| s.import(&mnemonic, &nickname, &passphrase)).await?;
    Ok(activate(&state, unlocked).await)
}

/// Re-verifies the passphrase before revealing the recovery phrase.
#[tauri::command]
pub(crate) async fn export_mnemonic(
    state: State<'_, AppState>,
    passphrase: String,
) -> CmdResult<String> {
    with_store(&state, move |s| {
        s.unlock(&passphrase).map(|u| u.seed.to_mnemonic())
    })
    .await
}

#[derive(Debug, Serialize)]
pub(crate) struct Conversation {
    peer_id: String,
    /// The name the user gave this contact.
    alias: Option<String>,
    last_message_at: u64,
    last: Option<UiMessage>,
    /// Incoming messages not yet read, among the latest [`UNREAD_WINDOW`].
    unread: usize,
}

/// How far back unread messages are counted; the list shows "99+" anyway.
const UNREAD_WINDOW: usize = 100;

/// Every conversation, most recent first, as the chat list shows it.
#[tauri::command]
pub(crate) async fn get_conversations(state: State<'_, AppState>) -> CmdResult<Vec<Conversation>> {
    let client = state.client().await?;
    let mut out = Vec::new();
    for contact in client.contacts().await.map_err(err)? {
        let recent = client
            .history(contact.peer, None, UNREAD_WINDOW)
            .await
            .map_err(err)?;
        let unread = recent
            .iter()
            .filter(|m| !m.outgoing && m.status != MessageStatus::Read)
            .count();
        out.push(Conversation {
            peer_id: contact.peer.to_hex(),
            alias: contact.alias,
            last_message_at: recent.first().map_or(0, |m| m.sent_at_ms),
            last: recent.first().map(ui::message),
            unread,
        });
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.last_message_at));
    Ok(out)
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

/// Ends the session with a contact and deletes the conversation.
#[tauri::command]
pub(crate) async fn delete_conversation(
    state: State<'_, AppState>,
    peer_id: String,
) -> CmdResult<()> {
    let peer = parse_peer(&peer_id)?;
    state.client().await?.forget_peer(peer).await.map_err(err)
}

#[tauri::command]
pub(crate) async fn get_history(
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
    Ok(history.iter().map(ui::message).collect())
}

#[tauri::command]
pub(crate) async fn clear_chat_history(state: State<'_, AppState>) -> CmdResult<()> {
    state.client().await?.clear_history().await.map_err(err)
}
