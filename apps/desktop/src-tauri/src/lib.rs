mod commands;
mod dto;
mod media_scheme;
mod session;
#[cfg(target_os = "android")]
mod shared_storage;
mod shell;
#[cfg(test)]
mod tests;

use commands::{chat, identity, link, media, qr, settings, transfer};
use tauri::{Emitter, Manager, Runtime};

#[cfg(mobile)]
#[tauri::mobile_entry_point]
pub fn mobile_entry_point() {
    if let Err(e) = run() {
        tracing::error!("application failed: {e}");
    }
}

#[expect(
    clippy::exit,
    clippy::disallowed_methods,
    reason = "tauri::generate_context! expands to process::exit for invalid embedded assets"
)]
pub fn run() -> tauri::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // First: a second launch hands its invite link to this one and quits.
    #[cfg(desktop)]
    let builder =
        tauri::Builder::default().plugin(tauri_plugin_single_instance::init(|app, _, _| {
            shell::show_main(app);
        }));
    #[cfg(mobile)]
    let builder = tauri::Builder::default().plugin(tauri_plugin_barcode_scanner::init());
    #[cfg(desktop)]
    let builder = builder.on_window_event(shell::on_close);
    wire(builder)
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_cypher_files::init())
        .setup(|app| {
            let paths = session::Paths::resolve(app.handle())?;
            app.manage(session::AppState::new(paths, session::tls_from_env()?));
            app.manage(shell::CloseToTray::default());
            #[cfg(desktop)]
            shell::install_tray(app)?;
            // Installers register cypher:// themselves; a dev build points
            // it at itself so invite links can be tried.
            #[cfg(all(desktop, debug_assertions))]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let _ = app.deep_link().register_all();
            }
            Ok(())
        })
        .on_page_load(|webview, _| allow_user_media(webview))
        .on_webview_event(|webview, event| {
            let tauri::WebviewEvent::DragDrop(drag) = event else {
                return;
            };
            let app = webview.app_handle();
            match drag {
                tauri::DragDropEvent::Enter { .. } => {
                    let _ = app.emit("cypher://files_dragging", true);
                }
                tauri::DragDropEvent::Drop { paths, .. } => {
                    let _ = app.emit("cypher://files_dragging", false);
                    transfer::files_dropped(app, paths.clone());
                }
                tauri::DragDropEvent::Leave => {
                    let _ = app.emit("cypher://files_dragging", false);
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
}

/// The commands and the media scheme, shared with the tests' mock runtime so
/// they exercise exactly what the webview can reach.
fn wire<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .register_asynchronous_uri_scheme_protocol(media_scheme::SCHEME, media_scheme::handle)
        .invoke_handler(tauri::generate_handler![
            identity::has_identity,
            identity::create_identity,
            identity::unlock_identity,
            identity::import_mnemonic,
            identity::erase_device,
            identity::lock,
            identity::export_mnemonic,
            identity::get_conversations,
            identity::get_history,
            identity::clear_chat_history,
            identity::rename_peer,
            identity::accept_contact,
            identity::set_blocked,
            identity::delete_conversation,
            settings::connect_to_gateway,
            settings::apply_anonymous_settings,
            settings::get_nickname,
            settings::reconnect,
            link::create_link,
            link::join_link,
            chat::send_message,
            chat::delete_message,
            chat::mark_read,
            chat::safety_number,
            transfer::browse_and_send,
            transfer::accept_file,
            transfer::cancel_transfer,
            transfer::file_saved,
            transfer::open_file,
            transfer::reveal_file,
            transfer::send_dropped,
            transfer::send_bytes,
            media::voice_start,
            media::voice_stop,
            media::voice_cancel,
            media::send_video_note,
            qr::generate_qr,
            shell::set_tray,
            shell::set_close_to_tray,
        ])
}

/// `WebKitGTK` denies camera and microphone requests unless the embedder
/// allows them; the other platforms ask the user themselves.
#[cfg(target_os = "linux")]
fn allow_user_media(webview: &tauri::Webview) {
    use webkit2gtk::glib::ObjectExt;
    use webkit2gtk::{PermissionRequestExt, UserMediaPermissionRequest, WebViewExt};

    let _ = webview.with_webview(|platform| {
        platform.inner().connect_permission_request(|_, request| {
            if request.is::<UserMediaPermissionRequest>() {
                request.allow();
                true
            } else {
                false
            }
        });
    });
}

#[cfg(not(target_os = "linux"))]
fn allow_user_media(_webview: &tauri::Webview) {}
