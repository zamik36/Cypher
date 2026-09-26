mod commands;
mod dto;
mod session;

use commands::{chat, identity, link, qr, settings, transfer};

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
            qr::generate_qr,
        ])
        .run(tauri::generate_context!())
        .expect("error running tauri application");
}
