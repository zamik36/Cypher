use cypher_core::{Event, ui};
use tauri::{AppHandle, Emitter};

/// Mirrors core events to the webview on `cypher://<channel>`.
pub fn emit(app: &AppHandle, event: &Event) {
    if let Some((channel, payload)) = ui::event(event) {
        let _ = app.emit(&format!("cypher://{channel}"), payload);
    }
}
