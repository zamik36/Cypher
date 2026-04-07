use cypher_client_core::onion::config::{AnonymousTransportConfig, TorSettings};
use cypher_client_core::onion::cover::PowerMode;

use crate::{current_api, AppState};

#[tauri::command]
pub async fn apply_anonymous_settings(
    state: tauri::State<'_, AppState>,
    enabled: bool,
    bridge_lines: Vec<String>,
) -> Result<(), String> {
    let api = current_api(&state).await;
    let config = AnonymousTransportConfig {
        power_mode: if enabled {
            PowerMode::Desktop
        } else {
            PowerMode::BatterySaver
        },
        target_count: 3,
        tor: TorSettings {
            enabled,
            bridge_lines,
        },
    };

    api.set_anonymous_transport_config(config)
        .await
        .map_err(|e| e.to_string())
}
