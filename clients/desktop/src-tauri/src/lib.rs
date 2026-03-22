mod commands;

use std::sync::Arc;

use cypher_client_core::api::{ClientApi, ClientEvent};
use cypher_common::PeerId;
use tauri::{Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::Mutex;

/// Shared application state injected into every Tauri command via `State<AppState>`.
pub struct AppState {
    pub api: Arc<ClientApi>,
    /// All connected remote peers.
    pub peers: Arc<Mutex<Vec<PeerId>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            api: Arc::new(ClientApi::new()),
            peers: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[cfg(mobile)]
#[tauri::mobile_entry_point]
pub fn mobile_entry_point() {
    run();
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(AppState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let state: tauri::State<'_, AppState> = app.state();
            let api = Arc::clone(&state.api);
            let peers = Arc::clone(&state.peers);

            tauri::async_runtime::spawn(async move {
                event_loop(api, peers, handle).await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::connect::connect_to_gateway,
            commands::link::create_link,
            commands::link::join_link,
            commands::chat::send_message,
            commands::chat::get_messages,
            commands::transfer::send_file,
            commands::transfer::browse_and_send,
            commands::transfer::accept_file,
            commands::transfer::get_transfers,
            commands::qr::generate_qr,
        ])
        .run(tauri::generate_context!())
        .expect("error running tauri application");
}

async fn event_loop(
    api: Arc<ClientApi>,
    peers: Arc<Mutex<Vec<PeerId>>>,
    handle: tauri::AppHandle,
) {
    while let Some(event) = api.next_event().await {
        match event {
            ClientEvent::Connected { peer_id } => {
                let _ = handle.emit("cypher://connected", peer_id_hex(&peer_id));
            }
            ClientEvent::Disconnected => {
                let _ = handle.emit("cypher://disconnected", ());
            }
            ClientEvent::PeerConnected { peer_id } => {
                // Add peer to the list and auto-initiate E2EE session.
                {
                    let mut list = peers.lock().await;
                    if !list.iter().any(|p| p.as_bytes() == peer_id.as_bytes()) {
                        list.push(peer_id.clone());
                    }
                }
                if let Err(e) = api.initiate_session(&peer_id).await {
                    tracing::warn!("auto initiate_session failed: {e}");
                }
                let _ = handle.emit("cypher://peer_connected", peer_id_hex(&peer_id));
            }
            ClientEvent::MessageReceived { from, plaintext } => {
                let text = String::from_utf8_lossy(&plaintext).into_owned();
                let payload = serde_json::json!({
                    "from": peer_id_hex(&from),
                    "text": text,
                    "timestamp": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                });
                let _ = handle.emit("cypher://message", payload);
            }
            ClientEvent::FileOffered { from, meta } => {
                let _ = handle
                    .notification()
                    .builder()
                    .title("File Offered")
                    .body(format!("{} ({} bytes)", meta.name, meta.size))
                    .show();
                let payload = serde_json::json!({
                    "from": peer_id_hex(&from),
                    "file_id": bytes_to_hex(&meta.file_id.to_vec()),
                    "name": meta.name,
                    "size": meta.size,
                    "chunks": meta.chunk_count,
                });
                let _ = handle.emit("cypher://file_offered", payload);
            }
            ClientEvent::FileProgress { file_id, progress } => {
                let payload = serde_json::json!({
                    "file_id": bytes_to_hex(&file_id),
                    "progress": progress,
                });
                let _ = handle.emit("cypher://file_progress", payload);
            }
            ClientEvent::FileComplete { file_id } => {
                let _ = handle
                    .notification()
                    .builder()
                    .title("Transfer Complete")
                    .body("File transfer finished successfully")
                    .show();
                let _ = handle.emit("cypher://file_complete", bytes_to_hex(&file_id));
            }
            ClientEvent::IceCandidateReceived { from, candidate } => {
                tracing::debug!(
                    from = bytes_to_hex(&from),
                    addr = %candidate.addr,
                    "remote ICE candidate received"
                );
                let payload = serde_json::json!({
                    "from": bytes_to_hex(&from),
                    "addr": format!("{}", candidate.addr),
                });
                let _ = handle.emit("cypher://ice_candidate", payload);
            }
            ClientEvent::Error(msg) => {
                let _ = handle.emit("cypher://error", msg);
            }
        }
    }
}

fn peer_id_hex(id: &PeerId) -> String {
    bytes_to_hex(id.as_bytes())
}

fn bytes_to_hex(b: &[u8]) -> String {
    b.iter().map(|byte| format!("{byte:02x}")).collect()
}
