use cypher_core::{CmdError, ErrorKind};
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::dto;
use crate::session::{AppState, CmdResult, Endpoint};

fn clean_bridges(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Starts (or restarts) the client against `addr` and returns our peer id.
#[tauri::command]
pub(crate) async fn connect_to_gateway<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    addr: String,
    anonymous: bool,
    bridges: Vec<String>,
) -> CmdResult<String> {
    let addr = addr.trim();
    cypher_transport::split_host_port(addr)
        .map_err(|e| CmdError::new(ErrorKind::Invalid, e.to_string()))?;
    let endpoint = Endpoint {
        gateway_addr: addr.to_owned(),
        anonymous,
        bridges: clean_bridges(bridges),
    };
    let me = state.connect(endpoint, dto::emitter(app.clone())).await?;
    client_ready(&app);
    Ok(me)
}

/// Switching anonymity changes the transport, so the session restarts.
#[tauri::command]
pub(crate) async fn apply_anonymous_settings<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    anonymous: bool,
    bridges: Vec<String>,
) -> CmdResult<()> {
    let endpoint = Endpoint {
        anonymous,
        bridges: clean_bridges(bridges),
        ..state.endpoint().await
    };
    state.connect(endpoint, dto::emitter(app.clone())).await?;
    client_ready(&app);
    Ok(())
}

/// Tells the UI that a client is up for its commands, online or not: sent
/// once the connect that started it is over, so a command that was told
/// `connecting` can be tried again.
fn client_ready<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit("cypher://client_ready", ());
}

/// Trusts the signing key the server showed in place of the one remembered.
#[tauri::command]
pub(crate) async fn trust_server_key(state: State<'_, AppState>) -> CmdResult<()> {
    state
        .client()
        .await?
        .command(cypher_core::Command::TrustServerKey)
        .await
        .map_err(CmdError::from)
}

/// Takes the session back after another device took it over.
#[tauri::command]
pub(crate) async fn reconnect(state: State<'_, AppState>) -> CmdResult<()> {
    state
        .client()
        .await?
        .reconnect()
        .await
        .map_err(CmdError::from)
}
