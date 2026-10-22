//! The identity: creating, unlocking, importing, locking and erasing it.

use cypher_client::{IdentityStore, Unlocked};
use tauri::State;

use crate::session::{AppState, CmdResult, err};

/// Argon2id is deliberately slow; keep it off the async runtime.
pub(super) async fn with_store<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(IdentityStore) -> Result<T, cypher_client::ClientError> + Send + 'static,
) -> CmdResult<T> {
    let store = IdentityStore::new(&state.paths().data);
    tokio::task::spawn_blocking(move || f(store))
        .await
        .map_err(err)?
        .map_err(err)
}

pub(super) async fn activate(state: &AppState, unlocked: Unlocked) -> String {
    let peer = unlocked.seed.derive_identity().peer_id().to_hex();
    state.set_identity(unlocked).await;
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

/// Locks the app: the client stops and the identity leaves memory until the
/// passphrase is typed again.
#[tauri::command]
pub(crate) async fn lock(state: State<'_, AppState>) -> CmdResult<()> {
    state.lock().await;
    Ok(())
}

/// Deletes the profile and everything kept with it from this device, for a
/// forgotten passphrase. Asks for none: without it nothing here is readable
/// anyway, and the recovery phrase brings the identity back.
#[tauri::command]
pub(crate) async fn erase_device(state: State<'_, AppState>) -> CmdResult<()> {
    state.lock().await;
    with_store(&state, |store| store.erase_device()).await
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
