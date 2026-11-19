use cypher_core::{Event, ui};
use tauri::{AppHandle, Emitter, Runtime};

/// Mirrors core events to the webview on `cypher://<channel>`. On Android a
/// received file is first moved to Downloads, so it is announced where the
/// user will find it.
pub(crate) fn emitter<R: Runtime>(app: AppHandle<R>) -> impl Fn(&Event) + Send + 'static {
    move |event| {
        #[cfg(target_os = "android")]
        if let Event::TransferComplete { file_id } = event {
            crate::shared_storage::publish_then_announce(app.clone(), *file_id);
            return;
        }
        emit(&app, event);
    }
}

pub(crate) fn emit<R: Runtime>(app: &AppHandle<R>, event: &Event) {
    if let Some((channel, payload)) = ui::event(event) {
        let _ = app.emit(&format!("cypher://{channel}"), payload);
    }
}
