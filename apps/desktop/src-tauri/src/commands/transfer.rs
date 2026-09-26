use std::path::{Path, PathBuf};

use cypher_core::{Command, MediaKind};
use cypher_types::FileId;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;

use super::chat::parse_peer;
use crate::session::{AppState, CmdResult, err};

#[derive(Serialize)]
pub struct TransferInfo {
    file_id: String,
    msg_id: String,
    file_name: String,
    total_size: u64,
    progress: f64,
    direction: &'static str,
    status: &'static str,
}

/// The backend owns path selection: the webview can never make the client
/// read an arbitrary file.
#[tauri::command]
pub async fn browse_and_send(
    app: AppHandle,
    state: State<'_, AppState>,
    peer_id: String,
) -> CmdResult<Vec<TransferInfo>> {
    let peer = parse_peer(&peer_id)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_files(move |files| {
        let _ = tx.send(files.unwrap_or_default());
    });
    let picked = rx.await.map_err(err)?;
    let client = state.client().await?;
    let mut out = Vec::with_capacity(picked.len());
    for file in picked {
        let path = file.into_path().map_err(err)?;
        let size = tokio::fs::metadata(&path).await.map_err(err)?.len();
        let (msg_id, file_id) = client
            .send_file(peer, &path, "application/octet-stream", MediaKind::File)
            .await
            .map_err(err)?;
        out.push(TransferInfo {
            file_id: file_id.to_hex(),
            msg_id: msg_id.to_hex(),
            file_name: display_name(&path),
            total_size: size,
            progress: 0.0,
            direction: "send",
            status: "active",
        });
    }
    Ok(out)
}

/// Saves into the user's downloads folder under the (already sanitized)
/// offered name, never overwriting an existing file.
#[tauri::command]
pub async fn accept_file(
    app: AppHandle,
    state: State<'_, AppState>,
    file_id: String,
) -> CmdResult<()> {
    let id = FileId::from_hex(&file_id).ok_or("invalid file id")?;
    let name = state
        .offers
        .lock()
        .map_err(err)?
        .remove(&id)
        .ok_or("unknown offer")?;
    let dir = app.path().download_dir().map_err(err)?;
    let dest = unique_path(&dir, &name);
    state
        .client()
        .await?
        .accept_file(id, dest)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn cancel_transfer(state: State<'_, AppState>, file_id: String) -> CmdResult<()> {
    let id = FileId::from_hex(&file_id).ok_or("invalid file id")?;
    state
        .client()
        .await?
        .command(Command::CancelTransfer { file_id: id })
        .await
        .map_err(err)
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(candidate)
}

#[cfg(test)]
mod tests {
    use super::unique_path;

    #[test]
    fn never_overwrites() {
        let dir = std::env::temp_dir().join(format!("cypher-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        std::fs::write(dir.join("a (1).txt"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (2).txt"));
        assert_eq!(unique_path(&dir, "b.txt"), dir.join("b.txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
