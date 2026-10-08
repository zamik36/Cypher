//! Several devices on one identity: a new device waits to be linked, a
//! device of the identity links or unlinks others.

use cypher_core::{Command, Event};
use tauri::State;

use super::identity::{activate, with_store};
use crate::session::{AppState, CmdResult, await_event, err};

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
    cypher_transport::split_host_port(addr).map_err(err)?;
    let waiting = cypher_client::provision(addr.to_owned(), state.tls(), &name);
    let offer = waiting.offer().to_owned();
    *state.linking.lock().await = Some(waiting);
    Ok(offer)
}

/// Waits until a device of the identity hands it over, keeps it under
/// `passphrase` and unlocks it: our peer id and nickname.
#[tauri::command]
pub(crate) async fn link_finish(
    state: State<'_, AppState>,
    passphrase: String,
) -> CmdResult<(String, String)> {
    let waiting = state
        .linking
        .lock()
        .await
        .take()
        .ok_or("not waiting to be linked")?;
    let linked = tokio::select! {
        linked = waiting.linked() => linked.map_err(err)?,
        () = state.link_cancel.notified() => return Err("cancelled".to_owned()),
    };
    let unlocked = with_store(&state, move |s| s.adopt(linked, &passphrase)).await?;
    let nickname = unlocked.nickname.clone();
    Ok((activate(&state, unlocked).await, nickname))
}

/// Stops waiting to be linked.
#[tauri::command]
pub(crate) async fn link_cancel(state: State<'_, AppState>) -> CmdResult<()> {
    state.link_cancel.notify_waiters();
    state.linking.lock().await.take();
    Ok(())
}

/// Links the new device whose offer the user scanned or pasted.
#[tauri::command]
pub(crate) async fn link_device(state: State<'_, AppState>, offer: String) -> CmdResult<()> {
    let (client, mut events) = state.client_and_events().await?;
    client
        .command(Command::LinkDevice { offer })
        .await
        .map_err(err)?;
    await_event(&mut events, |e| match e {
        Event::DeviceLinked { .. } => Some(Ok(())),
        Event::LinkFailed { reason } => Some(Err(format!("{reason:?}"))),
        _ => None,
    })
    .await
}

/// Takes another device off this identity's list.
#[tauri::command]
pub(crate) async fn unlink_device(state: State<'_, AppState>, device: u32) -> CmdResult<()> {
    state
        .client()
        .await?
        .command(Command::UnlinkDevice { device })
        .await
        .map_err(err)
}
