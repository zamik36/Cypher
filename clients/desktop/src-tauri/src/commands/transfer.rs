use std::path::Path;

use serde::{Deserialize, Serialize};
use tauri_plugin_dialog::DialogExt;

use crate::AppState;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TransferInfo {
    pub file_id: String,
    pub file_name: String,
    pub total_size: u64,
    pub progress: f64,
    pub direction: String, // "send" or "receive"
}

/// Offer a file to the currently connected peer.
///
/// Progress is reported via `cypher://file_progress` and `cypher://file_complete`
/// Tauri events.  Returns basic metadata so the UI can show the transfer
/// immediately at 0 % before the first progress event arrives.
#[tauri::command]
pub async fn send_file(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<TransferInfo, String> {
    let peer_id = state
        .peers
        .lock()
        .await
        .iter()
        .next()
        .cloned()
        .ok_or_else(|| "no peer connected".to_string())?;

    let api = state.api.lock().await;
    let meta = api
        .send_file(&peer_id, Path::new(&path))
        .await
        .map_err(|e| e.to_string())?;

    Ok(TransferInfo {
        file_id: bytes_to_hex(&meta.file_id.to_vec()),
        file_name: meta.name,
        total_size: meta.size as u64,
        progress: 0.0,
        direction: "send".to_string(),
    })
}

/// Accept an incoming file offer (identified by its hex file_id).
///
/// `dest_path` is the local filesystem path where the file will be assembled.
/// After this call chunks arrive and progress is reported via Tauri events.
#[tauri::command]
pub async fn accept_file(
    state: tauri::State<'_, AppState>,
    file_id: String,
    dest_path: String,
) -> Result<(), String> {
    let id_bytes = hex_decode(&file_id).map_err(|e| e.to_string())?;
    let api = state.api.lock().await;
    api.accept_file(&id_bytes, Path::new(&dest_path))
        .await
        .map_err(|e| e.to_string())
}

/// Open a native file dialog and send the selected file(s).
#[tauri::command]
pub async fn browse_and_send(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<TransferInfo>, String> {
    let files = app
        .dialog()
        .file()
        .set_title("Select files to send")
        .blocking_pick_files();

    let paths = match files {
        Some(paths) => paths,
        None => return Ok(Vec::new()),
    };

    let peer_id = state
        .peers
        .lock()
        .await
        .iter()
        .next()
        .cloned()
        .ok_or_else(|| "no peer connected".to_string())?;

    let api = state.api.lock().await;
    let mut result = Vec::new();
    for path_buf in paths {
        let path_str = path_buf.to_string();
        let meta = api
            .send_file(&peer_id, Path::new(&path_str))
            .await
            .map_err(|e| e.to_string())?;

        result.push(TransferInfo {
            file_id: bytes_to_hex(&meta.file_id.to_vec()),
            file_name: meta.name,
            total_size: meta.size as u64,
            progress: 0.0,
            direction: "send".to_string(),
        });
    }

    Ok(result)
}

/// Returns an empty list — transfer state is tracked via Tauri events.
#[tauri::command]
pub async fn get_transfers() -> Result<Vec<TransferInfo>, String> {
    Ok(Vec::new())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn bytes_to_hex(b: &[u8]) -> String {
    b.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_decode(hex: &str) -> cypher_common::Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return Err(cypher_common::Error::Protocol(
            "odd-length hex string".into(),
        ));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| cypher_common::Error::Protocol(format!("invalid hex at {i}")))
        })
        .collect()
}
