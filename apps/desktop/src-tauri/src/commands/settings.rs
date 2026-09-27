use tauri::{AppHandle, State};

use crate::session::{AppState, CmdResult, Endpoint, err};

fn clean_bridges(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Starts (or restarts) the client against `addr` and returns our peer id.
#[tauri::command]
pub(crate) async fn connect_to_gateway(
    app: AppHandle,
    state: State<'_, AppState>,
    addr: String,
    anonymous: bool,
    bridges: Vec<String>,
) -> CmdResult<String> {
    let addr = addr.trim();
    cypher_transport::split_host_port(addr).map_err(err)?;
    let endpoint = Endpoint {
        gateway_addr: addr.to_owned(),
        anonymous,
        bridges: clean_bridges(bridges),
    };
    state.connect(&app, endpoint).await
}

/// Switching anonymity changes the transport, so the session restarts.
#[tauri::command]
pub(crate) async fn apply_anonymous_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    anonymous: bool,
    bridges: Vec<String>,
) -> CmdResult<()> {
    let endpoint = Endpoint {
        anonymous,
        bridges: clean_bridges(bridges),
        ..state.endpoint().await
    };
    state.connect(&app, endpoint).await.map(|_| ())
}

#[tauri::command]
pub(crate) async fn get_nickname(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    Ok(state.nickname().await)
}
