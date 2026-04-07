mod commands;

use std::collections::HashSet;
use std::sync::Arc;

use cypher_client_core::api::{ClientApi, ClientEvent};
use cypher_common::PeerId;
use tauri::Emitter;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::{watch, Mutex, RwLock};

/// Shared application state injected into every Tauri command via `State<AppState>`.
pub struct AppState {
    /// The active client API. Commands clone the current Arc under a short read
    /// lock and then release it before awaiting network operations.
    pub api: RwLock<Arc<ClientApi>>,
    /// All connected remote peers.
    pub peers: Arc<Mutex<HashSet<PeerId>>>,
    /// Current nickname (set after identity unlock/create).
    pub nickname: Mutex<Option<String>>,
    /// Cancellation channel for the currently running event loop, if any.
    pub event_loop_cancel: Mutex<Option<watch::Sender<bool>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            api: RwLock::new(Arc::new(ClientApi::new())),
            peers: Arc::new(Mutex::new(HashSet::new())),
            nickname: Mutex::new(None),
            event_loop_cancel: Mutex::new(None),
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
            commands::identity::has_identity,
            commands::identity::create_identity,
            commands::identity::unlock_identity,
            commands::identity::export_mnemonic,
            commands::identity::import_mnemonic,
            commands::identity::get_conversations,
            commands::identity::get_conversation,
            commands::identity::get_history,
            commands::identity::clear_chat_history,
            commands::settings::apply_anonymous_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error running tauri application");
}

pub async fn current_api(state: &AppState) -> Arc<ClientApi> {
    state.api.read().await.clone()
}

pub async fn restart_event_loop(state: &AppState, handle: tauri::AppHandle) {
    let api = current_api(state).await;
    let peers = Arc::clone(&state.peers);
    let (cancel_tx, cancel_rx) = watch::channel(false);

    let previous_cancel = {
        let mut guard = state.event_loop_cancel.lock().await;
        guard.replace(cancel_tx)
    };

    if let Some(tx) = previous_cancel {
        let _ = tx.send(true);
    }

    tauri::async_runtime::spawn(async move {
        event_loop_wrapper(api, peers, handle, cancel_rx).await;
    });
}

async fn event_loop_wrapper(
    api: Arc<ClientApi>,
    peers: Arc<Mutex<HashSet<PeerId>>>,
    handle: tauri::AppHandle,
    mut cancel_rx: watch::Receiver<bool>,
) {
    loop {
        let event = tokio::select! {
            changed = cancel_rx.changed() => {
                if changed.is_ok() && *cancel_rx.borrow() {
                    break;
                }
                continue;
            }
            event = api.next_event() => {
                event
            }
        };
        let Some(event) = event else { break };
        handle_event(event, &api, &peers, &handle).await;
    }
}

async fn handle_event(
    event: ClientEvent,
    api: &Arc<ClientApi>,
    peers: &Arc<Mutex<HashSet<PeerId>>>,
    handle: &tauri::AppHandle,
) {
    match event {
        ClientEvent::Connected { peer_id } => {
            let _ = handle.emit("cypher://connected", peer_id_hex(&peer_id));
        }
        ClientEvent::Disconnected => {
            // Clear peer list on disconnect — sessions are invalid after reconnect.
            peers.lock().await.clear();
            let _ = handle.emit("cypher://disconnected", ());
        }
        ClientEvent::PeerConnected { peer_id } => {
            // Add peer (O(1) dedup via HashSet) and auto-initiate E2EE session.
            {
                let mut set = peers.lock().await;
                set.insert(peer_id.clone());
            }
            {
                if let Err(e) = api.initiate_session(&peer_id).await {
                    tracing::warn!("auto initiate_session failed: {e}");
                }
                // Auto-save conversation when a new peer connects.
                if let Some(store) = api.message_store() {
                    let _ = store.save_conversation(&peer_id, None);
                }
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
        ClientEvent::AnonymityLevelChanged { level } => {
            let payload = serde_json::json!({
                "level": level as u8,
                "label": level.to_string(),
                "description": level.description(),
            });
            let _ = handle.emit("cypher://anonymity_level", payload);
        }
        ClientEvent::Error(msg) => {
            let _ = handle.emit("cypher://error", msg);
        }
    }
}

fn peer_id_hex(id: &PeerId) -> String {
    bytes_to_hex(id.as_bytes())
}

fn bytes_to_hex(b: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        let _ = write!(s, "{byte:02x}");
    }
    s
}
