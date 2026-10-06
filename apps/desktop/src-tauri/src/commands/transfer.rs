use std::path::{Path, PathBuf};

use cypher_client::Client;
use cypher_core::{Command, MediaKind};
use cypher_types::{FileId, MsgId, PeerId};
use serde::Serialize;
use tauri::ipc::Request;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tauri_plugin_dialog::{DialogExt, FilePath};

use super::chat::parse_peer;
use super::ipc;
use crate::session::{AppState, CmdResult, err};

#[derive(Debug, Serialize)]
pub(crate) struct TransferInfo {
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
pub(crate) async fn browse_and_send<R: Runtime>(
    app: AppHandle<R>,
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
        let (msg_id, file_id, file_name, total_size) = send_pick(&app, &client, peer, file).await?;
        out.push(TransferInfo {
            file_id: file_id.to_hex(),
            msg_id: msg_id.to_hex(),
            file_name,
            total_size,
            progress: 0.0,
            direction: "send",
            status: "active",
        });
    }
    Ok(out)
}

/// Files dropped on the window, held for the UI to send to the open chat:
/// the paths come from the drop itself, never from the webview.
#[derive(Default)]
pub(crate) struct Dropped {
    id: u64,
    paths: Vec<PathBuf>,
}

#[cfg(test)]
impl Dropped {
    pub(crate) fn id_for_tests(&self) -> u64 {
        self.id
    }
}

#[derive(Clone, Serialize)]
struct DropInfo {
    id: u64,
    names: Vec<String>,
}

/// Remembers a drop and tells the UI, which answers with `send_dropped`.
pub(crate) fn files_dropped<R: Runtime>(app: &AppHandle<R>, paths: Vec<PathBuf>) {
    let state = app.state::<AppState>();
    let Ok(mut dropped) = state.dropped.lock() else {
        return;
    };
    dropped.id += 1;
    let info = DropInfo {
        id: dropped.id,
        names: paths.iter().map(|p| display_name(p)).collect(),
    };
    dropped.paths = paths;
    drop(dropped);
    let _ = app.emit("cypher://files_dropped", info);
}

/// Sends the files of drop `id` to `peer_id`; each drop is sent once.
#[tauri::command]
pub(crate) async fn send_dropped<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    peer_id: String,
    id: u64,
) -> CmdResult<Vec<TransferInfo>> {
    let peer = parse_peer(&peer_id)?;
    let paths = {
        let mut dropped = state.dropped.lock().map_err(err)?;
        if dropped.id != id || dropped.paths.is_empty() {
            return Err("this drop is gone".into());
        }
        std::mem::take(&mut dropped.paths)
    };
    let client = state.client().await?;
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        // Folders and the like are skipped, not fatal.
        if !tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_file()) {
            continue;
        }
        let (msg_id, file_id, file_name, total_size) =
            send_pick(&app, &client, peer, FilePath::Path(path)).await?;
        out.push(TransferInfo {
            file_id: file_id.to_hex(),
            msg_id: msg_id.to_hex(),
            file_name,
            total_size,
            progress: 0.0,
            direction: "send",
            status: "active",
        });
    }
    Ok(out)
}

/// A fresh name for a staged copy.
fn staging_name() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos}-{n}.tmp")
}

/// Largest file sent from memory (a pasted picture); bigger ones go by path.
const MAX_SENT_BYTES: usize = 50 << 20;

/// Sends bytes the webview holds, such as a pasted picture, as a file:
/// staged in the app's folder, then deleted by the client once sent.
#[tauri::command]
pub(crate) async fn send_bytes(
    state: State<'_, AppState>,
    request: Request<'_>,
) -> CmdResult<TransferInfo> {
    let headers = request.headers();
    let peer = parse_peer(&ipc::header(headers, "x-peer")?)?;
    let name = ipc::header(headers, "x-name")?;
    let mime = ipc::header(headers, "x-mime")?;
    let data = ipc::bytes(request.body(), MAX_SENT_BYTES, "file")?;
    let dir = state.paths().data.join("staging");
    tokio::fs::create_dir_all(&dir).await.map_err(err)?;
    let staged = dir.join(staging_name());
    tokio::fs::write(&staged, &data).await.map_err(err)?;
    let (msg_id, file_id) = state
        .client()
        .await?
        .send_staged_file(peer, &staged, &name, &mime)
        .await
        .map_err(err)?;
    Ok(TransferInfo {
        file_id: file_id.to_hex(),
        msg_id: msg_id.to_hex(),
        file_name: name,
        total_size: data.len() as u64,
        progress: 0.0,
        direction: "send",
        status: "active",
    })
}

/// Sends one picked file: a path as it is, a `content://` pick (Android)
/// through a copy the client deletes when done. Returns the ids, the name
/// it went by and its size.
pub(crate) async fn send_pick<R: Runtime>(
    app: &AppHandle<R>,
    client: &Client,
    peer: PeerId,
    file: FilePath,
) -> CmdResult<(MsgId, FileId, String, u64)> {
    let path = match file {
        FilePath::Path(path) => path,
        FilePath::Url(url) => match url.to_file_path() {
            Ok(path) => path,
            Err(()) => return send_content(app, client, peer, url.as_str()).await,
        },
    };
    let size = tokio::fs::metadata(&path).await.map_err(err)?.len();
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let (msg_id, file_id) = client
        .send_file(peer, &path, mime.essence_str(), MediaKind::File)
        .await
        .map_err(err)?;
    Ok((msg_id, file_id, display_name(&path), size))
}

#[cfg(target_os = "android")]
async fn send_content<R: Runtime>(
    app: &AppHandle<R>,
    client: &Client,
    peer: PeerId,
    uri: &str,
) -> CmdResult<(MsgId, FileId, String, u64)> {
    use tauri::Manager;
    let (app, uri) = (app.clone(), uri.to_owned());
    let staged = tauri::async_runtime::spawn_blocking(move || {
        app.state::<tauri_plugin_cypher_files::Files<R>>()
            .stage(&uri)
    })
    .await
    .map_err(err)??;
    let size = tokio::fs::metadata(&staged.path).await.map_err(err)?.len();
    let (msg_id, file_id) = client
        .send_staged_file(peer, Path::new(&staged.path), &staged.name, &staged.mime)
        .await
        .map_err(err)?;
    Ok((msg_id, file_id, staged.name, size))
}

#[cfg(not(target_os = "android"))]
#[expect(clippy::unused_async, reason = "the Android version awaits the plugin")]
async fn send_content<R: Runtime>(
    _app: &AppHandle<R>,
    _client: &Client,
    _peer: PeerId,
    _uri: &str,
) -> CmdResult<(MsgId, FileId, String, u64)> {
    Err("unsupported file location".into())
}

/// Where a finished file is, if it is still there. A `content://` URI is
/// taken at its word; a path must still exist.
async fn saved_location(state: &AppState, file_id: &str) -> CmdResult<Option<String>> {
    let id = FileId::from_hex(file_id).ok_or("invalid file id")?;
    let location = state.client().await?.saved_file(id).await.map_err(err)?;
    Ok(location.filter(|l| l.starts_with("content://") || Path::new(l).exists()))
}

/// Whether a finished file can be opened from this device.
#[tauri::command]
pub(crate) async fn file_saved(state: State<'_, AppState>, file_id: String) -> CmdResult<bool> {
    Ok(saved_location(&state, &file_id).await?.is_some())
}

/// Opens a finished file with the app the system picks for it.
#[tauri::command]
pub(crate) async fn open_file<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    file_id: String,
) -> CmdResult<()> {
    let location = saved_location(&state, &file_id).await?.ok_or("not_saved")?;
    open_location(app, location).await
}

#[cfg(target_os = "android")]
async fn open_location<R: Runtime>(app: AppHandle<R>, location: String) -> CmdResult<()> {
    use tauri::Manager;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<tauri_plugin_cypher_files::Files<R>>()
            .open(&location)
    })
    .await
    .map_err(err)?
}

#[cfg(not(target_os = "android"))]
#[expect(clippy::unused_async, reason = "the Android version awaits the plugin")]
async fn open_location<R: Runtime>(_app: AppHandle<R>, location: String) -> CmdResult<()> {
    if runs_code(Path::new(&location)) {
        // Opening would run it: the user can still do so from the folder.
        return Err("unsafe_type".into());
    }
    tauri_plugin_opener::open_path(&location, None::<&str>).map_err(err)
}

/// Shows a finished file in the system's file manager (desktop only).
#[tauri::command]
pub(crate) async fn reveal_file(state: State<'_, AppState>, file_id: String) -> CmdResult<()> {
    let location = saved_location(&state, &file_id).await?.ok_or("not_saved")?;
    reveal_location(&location)
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn reveal_location(location: &str) -> CmdResult<()> {
    tauri_plugin_opener::reveal_item_in_dir(location).map_err(err)
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn reveal_location(_location: &str) -> CmdResult<()> {
    Err("unsupported".into())
}

/// Extensions the system runs rather than shows: never opened from a chat.
/// (Android hands opening to the app the user picks.)
#[cfg(not(target_os = "android"))]
const RUNS_CODE: &[&str] = &[
    "app",
    "appimage",
    "application",
    "bat",
    "cmd",
    "com",
    "command",
    "cpl",
    "deb",
    "desktop",
    "dll",
    "exe",
    "gadget",
    "hta",
    "jar",
    "js",
    "jse",
    "lnk",
    "msc",
    "msi",
    "msix",
    "pif",
    "pkg",
    "ps1",
    "reg",
    "rpm",
    "scf",
    "scr",
    "sh",
    "url",
    "vb",
    "vbe",
    "vbs",
    "ws",
    "wsf",
    "wsh",
];

#[cfg(not(target_os = "android"))]
fn runs_code(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RUNS_CODE.contains(&e.to_ascii_lowercase().as_str()))
}

/// Saves into the user's downloads folder under the (already sanitized)
/// offered name, never overwriting an existing file.
#[tauri::command]
pub(crate) async fn accept_file(state: State<'_, AppState>, file_id: String) -> CmdResult<()> {
    let id = FileId::from_hex(&file_id).ok_or("invalid file id")?;
    let name = state
        .offers
        .lock()
        .map_err(err)?
        .remove(&id)
        .ok_or("unknown offer")?;
    let dir = state
        .paths()
        .downloads
        .as_deref()
        .ok_or("no downloads folder")?;
    let dest = unique_path(dir, &name).ok_or("too many files with this name")?;
    state
        .client()
        .await?
        .accept_file(id, dest)
        .await
        .map_err(err)
}

#[tauri::command]
pub(crate) async fn cancel_transfer(state: State<'_, AppState>, file_id: String) -> CmdResult<()> {
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

/// Most `name (n).ext` variants tried before giving up.
const MAX_NAME_VARIANTS: u32 = 10_000;

/// `name`, or `name (n).ext` when taken; `None` rather than overwrite.
fn unique_path(dir: &Path, name: &str) -> Option<PathBuf> {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return Some(candidate);
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (1..=MAX_NAME_VARIANTS)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{runs_code, unique_path};

    #[test]
    fn programs_are_told_apart_from_documents() {
        for name in [
            "setup.exe",
            "INVOICE.PDF.EXE",
            "run.bat",
            "link.lnk",
            "x.ps1",
            "a.sh",
        ] {
            assert!(runs_code(Path::new(name)), "{name}");
        }
        for name in ["photo.jpg", "report.pdf", "notes", "archive.tar.gz", ".exe"] {
            assert!(!runs_code(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn never_overwrites() {
        let dir = std::env::temp_dir().join(format!("cypher-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        std::fs::write(dir.join("a (1).txt"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), Some(dir.join("a (2).txt")));
        assert_eq!(unique_path(&dir, "b.txt"), Some(dir.join("b.txt")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
