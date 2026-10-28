//! Several devices on one identity: a new device waits to be linked, a
//! device of the identity links or unlinks others.

use cypher_core::{CmdError, Command, CommandId, ErrorKind, Event};
use tauri::State;

use super::identity::{activate, with_store};
use crate::session::{AppState, CmdResult, await_event};

/// A name for this device to suggest; the user may change it.
#[tauri::command]
pub(crate) fn device_name() -> String {
    if cfg!(target_os = "android") {
        return "Android".to_owned();
    }
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|var| std::env::var(var).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Desktop".to_owned())
}

/// Starts waiting to be linked as a device called `name`, through the
/// gateway at `addr`; returns the offer to show.
#[tauri::command]
pub(crate) async fn link_start(
    state: State<'_, AppState>,
    addr: String,
    name: String,
) -> CmdResult<String> {
    let addr = addr.trim();
    cypher_transport::split_host_port(addr)
        .map_err(|e| CmdError::new(ErrorKind::Invalid, e.to_string()))?;
    let waiting = cypher_client::provision(addr.to_owned(), state.tls(), &name);
    let offer = waiting.offer().to_owned();
    state.linking.start(waiting).await;
    Ok(offer)
}

/// Waits until a device of the identity hands it over, keeps it under
/// `passphrase` and unlocks it: our peer id and nickname.
#[tauri::command]
pub(crate) async fn link_finish(
    state: State<'_, AppState>,
    passphrase: String,
) -> CmdResult<(String, String)> {
    let (waiting, cancel) = state.linking.take().await.ok_or(ErrorKind::NotLinking)?;
    let linked = tokio::select! {
        linked = waiting.linked() => linked?,
        () = cancel.cancelled() => return Err(ErrorKind::Cancelled.into()),
    };
    let unlocked = with_store(&state, move |s| s.adopt(linked, &passphrase)).await?;
    let nickname = unlocked.nickname.clone();
    Ok((activate(&state, unlocked).await, nickname))
}

/// Stops waiting to be linked.
#[tauri::command]
pub(crate) async fn link_cancel(state: State<'_, AppState>) -> CmdResult<()> {
    state.linking.cancel().await;
    Ok(())
}

/// Links the new device whose offer the user scanned or pasted; with
/// `history`, it also gets the message history.
#[tauri::command]
pub(crate) async fn link_device(
    state: State<'_, AppState>,
    offer: String,
    history: Option<bool>,
) -> CmdResult<()> {
    let (client, mut events) = state.client_and_events().await?;
    let history = history.unwrap_or(false);
    let id = state.command_id();
    client
        .command(Command::LinkDevice { id, offer, history })
        .await?;
    await_event(&mut events, device_linked(id)).await
}

/// The answer to `LinkDevice` `id`; a failure of another change to the
/// device list is not its answer.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "picks one reply among all the client's events"
)]
pub(crate) fn device_linked(id: CommandId) -> impl FnMut(&Event) -> Option<CmdResult<()>> {
    move |e| match e {
        Event::DeviceLinked { id: i, .. } if *i == id => Some(Ok(())),
        Event::LinkFailed {
            id: Some(i),
            reason,
        } if *i == id => Some(Err((*reason).into())),
        _ => None,
    }
}

/// Takes another device off this identity's list.
#[tauri::command]
pub(crate) async fn unlink_device(state: State<'_, AppState>, device: u32) -> CmdResult<()> {
    state
        .client()
        .await?
        .command(Command::UnlinkDevice { device })
        .await
        .map_err(CmdError::from)
}

/// Stops handing over, or receiving, the message history.
#[tauri::command]
pub(crate) async fn cancel_history(state: State<'_, AppState>) -> CmdResult<()> {
    state
        .client()
        .await?
        .command(Command::CancelHistory)
        .await
        .map_err(CmdError::from)
}

/// Puts this device on a full list in place of `device`.
#[tauri::command]
pub(crate) async fn replace_device(state: State<'_, AppState>, device: u32) -> CmdResult<()> {
    state
        .client()
        .await?
        .command(Command::ReplaceDevice { device })
        .await
        .map_err(CmdError::from)
}
