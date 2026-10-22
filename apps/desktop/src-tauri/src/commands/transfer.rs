use std::fs::OpenOptions;
use std::io;
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

impl TransferInfo {
    /// The card of a file this device just offered: active, nothing sent yet.
    fn outgoing(msg_id: MsgId, file_id: FileId, file_name: String, total_size: u64) -> Self {
        Self {
            file_id: file_id.to_hex(),
            msg_id: msg_id.to_hex(),
            file_name,
            total_size,
            progress: 0.0,
            direction: "send",
            status: "active",
        }
    }
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
    send_all(&app, &client, peer, picked).await
}

/// Sends `files` to `peer` in order; the cards of the files sent. A file
/// that cannot be sent does not stop the others, whose cards the UI needs
/// to follow them. When none went, the first error is the answer; when
/// some did, each file left out is told to the UI as an error event,
/// `<name>: <reason>`.
pub(crate) async fn send_all<R: Runtime>(
    app: &AppHandle<R>,
    client: &Client,
    peer: PeerId,
    files: Vec<FilePath>,
) -> CmdResult<Vec<TransferInfo>> {
    let mut sent = Vec::with_capacity(files.len());
    let mut failed = Vec::new();
    for file in files {
        let name = file_label(&file);
        match send_pick(app, client, peer, file).await {
            Ok((msg_id, file_id, file_name, total_size)) => sent.push(TransferInfo::outgoing(
                msg_id, file_id, file_name, total_size,
            )),
            Err(e) => {
                tracing::warn!(error = %e, "a file of a batch was not sent");
                failed.push((name, e));
            }
        }
    }
    let mut failed = failed.into_iter();
    if sent.is_empty()
        && let Some((_, e)) = failed.next()
    {
        return Err(e);
    }
    for (name, e) in failed {
        let _ = app.emit("cypher://error", format!("{name}: {e}"));
    }
    Ok(sent)
}

/// How a picked file is named to the user.
fn file_label(file: &FilePath) -> String {
    match file {
        FilePath::Path(path) => display_name(path),
        FilePath::Url(url) => url.to_string(),
    }
}

/// Files dropped on the window, held for the UI to send to the open chat:
/// the paths come from the drop itself, never from the webview.
#[derive(Default)]
pub(crate) struct Dropped {
    id: u64,
    paths: Vec<PathBuf>,
}

impl Dropped {
    /// Forgets the files; the id keeps counting, so an old drop's id never
    /// names a later one.
    pub(crate) fn forget(&mut self) {
        self.paths.clear();
    }

    #[cfg(test)]
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
    let Ok(mut dropped) = state.transfers.dropped.lock() else {
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
        let mut dropped = state.transfers.dropped.lock().map_err(err)?;
        if dropped.id != id || dropped.paths.is_empty() {
            return Err("this drop is gone".into());
        }
        std::mem::take(&mut dropped.paths)
    };
    let client = state.client().await?;
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        // Folders and the like are skipped, not fatal.
        if tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_file()) {
            files.push(FilePath::Path(path));
        }
    }
    send_all(&app, &client, peer, files).await
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
    // Before anything is written: without a client, nothing is staged.
    let client = state.client().await?;
    let dir = state.paths().data.join("staging");
    tokio::fs::create_dir_all(&dir).await.map_err(err)?;
    let staged = dir.join(staging_name());
    let size = data.len() as u64;
    // An owned buffer (decoded on Android) moves to the writing thread; a
    // borrowed one is copied, as the write outlives the request.
    if let Err(e) = tokio::fs::write(&staged, data.into_owned()).await {
        discard(&staged).await;
        return Err(err(e));
    }
    let (msg_id, file_id) = send_staged(&client, peer, &staged, &name, &mime).await?;
    Ok(TransferInfo::outgoing(msg_id, file_id, name, size))
}

/// Sends `staged`, a copy made only for sending, which the client takes
/// over and deletes once sent; if sending fails, the copy is deleted here.
async fn send_staged(
    client: &Client,
    peer: PeerId,
    staged: &Path,
    name: &str,
    mime: &str,
) -> CmdResult<(MsgId, FileId)> {
    let sent = client.send_staged_file(peer, staged, name, mime).await;
    if sent.is_err() {
        discard(staged).await;
    }
    sent.map_err(err)
}

/// Deletes what is left of a staged copy; one already gone is fine.
async fn discard(staged: &Path) {
    let _ = tokio::fs::remove_file(staged).await;
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
    .map_err(err)?
    .map_err(err)?;
    let path = Path::new(&staged.path);
    let size = match tokio::fs::metadata(path).await {
        Ok(meta) => meta.len(),
        Err(e) => {
            discard(path).await;
            return Err(err(e));
        }
    };
    let (msg_id, file_id) = send_staged(client, peer, path, &staged.name, &staged.mime).await?;
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
    .map_err(err)
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
/// offered name, never overwriting an existing file. The offer stays open
/// if accepting fails.
#[tauri::command]
pub(crate) async fn accept_file(state: State<'_, AppState>, file_id: String) -> CmdResult<()> {
    let id = FileId::from_hex(&file_id).ok_or("invalid file id")?;
    let offers = &state.transfers.offers;
    let (name, ticket) = offers.take(&id).ok_or("unknown offer")?;
    let accepted = accept_as(&state, id, &name, ticket).await;
    if accepted.is_err() {
        offers.restore(id, name, ticket);
    }
    accepted
}

async fn accept_as(state: &AppState, id: FileId, name: &str, ticket: u64) -> CmdResult<()> {
    let dir = state
        .paths()
        .downloads
        .clone()
        .ok_or("no downloads folder")?;
    let client = state.client().await?;
    let name = name.to_owned();
    let dest = tokio::task::spawn_blocking(move || reserve_download(&dir, &name))
        .await
        .map_err(err)?
        .map_err(err)?;
    // Noted before the client hears of it: its failure may come at once.
    state.transfers.offers.accepted(id, dest.clone(), ticket);
    if let Err(e) = client.accept_file(id, dest.clone()).await {
        release_reservation(&dest).await;
        return Err(err(e));
    }
    Ok(())
}

/// Deletes a name [`reserve_download`] held if it is still what that left:
/// an empty regular file. A file with anything in it is the user's and stays.
pub(crate) async fn release_reservation(path: &Path) {
    let reserved = tokio::fs::symlink_metadata(path)
        .await
        .is_ok_and(|m| m.is_file() && m.len() == 0);
    if reserved {
        let _ = tokio::fs::remove_file(path).await;
    }
}

#[tauri::command]
pub(crate) async fn cancel_transfer(state: State<'_, AppState>, file_id: String) -> CmdResult<()> {
    let id = FileId::from_hex(&file_id).ok_or("invalid file id")?;
    state
        .client()
        .await?
        .command(Command::CancelTransfer { file_id: id })
        .await
        .map_err(err)?;
    state.transfers.offers.forget(&id);
    Ok(())
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Most `name (n).ext` variants tried before giving up.
const MAX_NAME_VARIANTS: u32 = 10_000;

/// Creates `name` in `dir`, or `name (n).ext` when taken, and returns its
/// path. The empty file holds the name until the transfer opens it, so two
/// transfers of one name never share a file; whatever is already there, a
/// link included, is left alone.
fn reserve_download(dir: &Path, name: &str) -> io::Result<PathBuf> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    for n in 0..=MAX_NAME_VARIANTS {
        let path = if n == 0 {
            dir.join(name)
        } else {
            dir.join(format!("{stem} ({n}){ext}"))
        };
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many files with this name",
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    use super::{release_reservation, reserve_download, runs_code};

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
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        std::fs::write(dir.join("a (1).txt"), b"y").unwrap();
        assert_eq!(
            reserve_download(dir, "a.txt").unwrap(),
            dir.join("a (2).txt")
        );
        assert_eq!(reserve_download(dir, "b.txt").unwrap(), dir.join("b.txt"));
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"x");
        assert_eq!(std::fs::read(dir.join("a (1).txt")).unwrap(), b"y");
        assert_eq!(reserve_download(dir, ".env").unwrap(), dir.join(".env"));
        assert_eq!(reserve_download(dir, ".env").unwrap(), dir.join(".env (1)"));
    }

    /// Accepts of one name, even at the same moment, each get a file
    /// of their own, so cancelling one never deletes another's.
    #[test]
    #[expect(
        clippy::needless_collect,
        reason = "every thread must be running before the first join"
    )]
    fn same_name_downloads_get_their_own_files() {
        let dir = tempfile::tempdir().unwrap();
        let reserved: Vec<PathBuf> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| reserve_download(dir.path(), "report.pdf").unwrap()))
                .collect();
            workers.into_iter().map(|w| w.join().unwrap()).collect()
        });
        let distinct: HashSet<&PathBuf> = reserved.iter().collect();
        assert_eq!(distinct.len(), reserved.len(), "two transfers, one file");
        assert!(reserved.iter().all(|p| p.is_file()), "each name is held");
    }

    /// Only an untouched reservation is deleted, never a file with data.
    #[tokio::test]
    async fn a_released_name_goes_only_while_empty() {
        let dir = tempfile::tempdir().unwrap();
        let empty = reserve_download(dir.path(), "a.txt").unwrap();
        let filled = reserve_download(dir.path(), "a.txt").unwrap();
        std::fs::write(&filled, b"received").unwrap();
        release_reservation(&empty).await;
        release_reservation(&filled).await;
        assert!(!empty.exists());
        assert_eq!(std::fs::read(&filled).unwrap(), b"received");
    }

    /// A link under the offered name, dangling or not, counts as
    /// taken; nothing is created where it points.
    #[test]
    fn a_link_under_the_name_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside").join("autostart.desktop");
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        let link = dir.path().join("notes.txt");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&outside, &link);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&outside, &link);
        if let Err(e) = made {
            eprintln!("no symlinks here ({e}); skipped");
            return;
        }
        let picked = reserve_download(dir.path(), "notes.txt").unwrap();
        assert_eq!(picked, dir.path().join("notes (1).txt"));
        assert!(!outside.exists(), "created at the link's target");
    }
}
