use crate::AppState;

/// Connect to the P2P gateway over TLS.
///
/// Uses the system's trusted CA roots. For development with a self-signed
/// certificate on localhost, the cert must be added to the OS trust store,
/// or you can call `connect_to_gateway_dev` (TODO: add that variant).
#[tauri::command]
pub async fn connect_to_gateway(
    state: tauri::State<'_, AppState>,
    addr: String,
) -> Result<(), String> {
    state
        .api
        .connect_to_gateway(&addr)
        .await
        .map_err(|e| e.to_string())
}
