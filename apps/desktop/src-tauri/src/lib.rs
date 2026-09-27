mod commands;
mod dto;
mod media_scheme;
mod session;

use commands::{chat, identity, link, media, qr, settings, transfer};

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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(session::AppState::default())
        .register_asynchronous_uri_scheme_protocol(media_scheme::SCHEME, media_scheme::handle)
        .on_page_load(|webview, _| allow_user_media(webview))
        .invoke_handler(tauri::generate_handler![
            identity::has_identity,
            identity::create_identity,
            identity::unlock_identity,
            identity::import_mnemonic,
            identity::export_mnemonic,
            identity::get_conversations,
            identity::get_history,
            identity::clear_chat_history,
            settings::connect_to_gateway,
            settings::apply_anonymous_settings,
            settings::get_nickname,
            link::create_link,
            link::join_link,
            chat::send_message,
            chat::mark_read,
            transfer::browse_and_send,
            transfer::accept_file,
            transfer::cancel_transfer,
            media::voice_start,
            media::voice_stop,
            media::voice_cancel,
            media::send_video_note,
            qr::generate_qr,
        ])
        .run(tauri::generate_context!())
        .expect("error running tauri application");
}

/// WebKitGTK denies camera and microphone requests unless the embedder
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
