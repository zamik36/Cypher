//! Android: received files leave app storage for Downloads/Cypher, where
//! the user and other apps can find them.

use std::path::Path;

use cypher_core::Event;
use cypher_types::FileId;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_cypher_files::Files;

use crate::dto;
use crate::session::{AppState, CmdResult, err};

/// Publishes a received file, then tells the webview it is complete. A file
/// that cannot be published stays in app storage and still opens.
pub(crate) fn publish_then_announce<R: Runtime>(app: AppHandle<R>, file_id: FileId) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = publish(&app, file_id).await {
            tracing::warn!("a received file stays in app storage: {e}");
        }
        dto::emit(&app, &Event::TransferComplete { file_id });
    });
}

async fn publish<R: Runtime>(app: &AppHandle<R>, file_id: FileId) -> CmdResult<()> {
    let state = app.state::<AppState>();
    let Some(downloads) = state.paths().downloads.clone() else {
        return Ok(());
    };
    let client = state.client().await?;
    let Some(location) = client.saved_file(file_id).await.map_err(err)? else {
        return Ok(());
    };
    // Only what was received into app storage; sent files stay put.
    if !Path::new(&location).starts_with(&downloads) {
        return Ok(());
    }
    let name = Path::new(&location)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let handle = app.clone();
    let uri = tauri::async_runtime::spawn_blocking(move || {
        handle.state::<Files<R>>().publish(&location, &name, "")
    })
    .await
    .map_err(err)??;
    client.move_saved_file(file_id, uri).await.map_err(err)
}
