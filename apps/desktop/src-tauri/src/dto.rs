use cypher_core::{Event, ui};
use tauri::{AppHandle, Emitter, Runtime};

/// Mirrors core events to the webview on `cypher://<channel>`.
pub(crate) fn emitter<R: Runtime>(app: AppHandle<R>) -> impl Fn(&Event) + Send + 'static {
    move |event| {
        if let Some((channel, payload)) = ui::event(event) {
            let _ = app.emit(&format!("cypher://{channel}"), payload);
        }
    }
}
