// Matches on our own and Tauri's enums name every variant, so a new one
// is looked at where it matters.
#![warn(clippy::wildcard_enum_match_arm)]

mod commands;
mod dto;
mod ephemeral;
mod media_scheme;
mod session;
#[cfg(target_os = "android")]
mod shared_storage;
mod shell;
#[cfg(test)]
mod tests;

use commands::{chat, devices, identity, link, media, push, qr, settings, transfer};
use tauri::{Emitter, Manager, Runtime};

#[cfg(mobile)]
#[tauri::mobile_entry_point]
pub fn mobile_entry_point() {
    if let Err(e) = run() {
        tracing::error!("application failed: {e}");
    }
}

pub fn run() -> tauri::Result<()> {
    if let Err(e) = init_logging() {
        // Whoever set it up still gets this.
        tracing::warn!(error = %e, "logging was already set up; keeping it");
    }
    // Arti reaches the relay with TLS configs built from the process-wide
    // provider; the app's own configs carry theirs. Installed once, here,
    // before anything can use TLS.
    let _ = rustls::crypto::ring::default_provider().install_default();
    wire(with_plugins())
        .setup(setup)
        .on_page_load(|webview, _| allow_user_media(webview))
        .on_webview_event(on_webview_event)
        .build(context())?
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                shut_down(app);
            }
        });
    Ok(())
}

/// How long the app waits on exit for the client to stop.
const EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// On exit: closes the microphone, stops the client, so its storage is
/// flushed and its connections closed, and drops the unlocked identity;
/// a stop that hangs does not hold the exit beyond [`EXIT_GRACE`]. Blocks:
/// called from the event loop, outside the async runtime.
fn shut_down<R: Runtime>(app: &tauri::AppHandle<R>) {
    let Some(state) = app.try_state::<session::AppState>() else {
        return;
    };
    tauri::async_runtime::block_on(async {
        if tokio::time::timeout(EXIT_GRACE, state.lock())
            .await
            .is_err()
        {
            tracing::warn!("the client did not stop before exit");
        }
    });
}

/// Logs as `RUST_LOG` asks; an error, not a panic, when logging is
/// already set up.
fn init_logging() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
}

/// The builder with the platform's plugins and the app's own.
fn with_plugins() -> tauri::Builder<tauri::Wry> {
    // First: a second launch hands its invite link to this one and quits.
    #[cfg(desktop)]
    let builder =
        tauri::Builder::default().plugin(tauri_plugin_single_instance::init(|app, _, _| {
            shell::show_main(app);
        }));
    #[cfg(mobile)]
    let builder = tauri::Builder::default().plugin(tauri_plugin_barcode_scanner::init());
    // Signed updates from the releases; the key they must carry is in
    // tauri.conf.json.
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .on_window_event(shell::on_close);
    builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_cypher_files::init())
        .plugin(tauri_plugin_cypher_push::init())
}

/// The app's state, from where the platform keeps the app's files, and on
/// desktops the tray.
fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let paths = session::Paths::resolve(app.handle())?;
    app.manage(session::AppState::new(paths, session::tls_from_env()?));
    app.manage(shell::CloseToTray::default());
    #[cfg(desktop)]
    shell::install_tray(app)?;
    // Installers register cypher:// themselves; a dev build points it at
    // itself so invite links can be tried.
    #[cfg(all(desktop, debug_assertions))]
    {
        use tauri_plugin_deep_link::DeepLinkExt;
        let _ = app.deep_link().register_all();
    }
    Ok(())
}

/// Files dragged over the window: the UI shows where to drop them, and a
/// drop is held for the open chat.
fn on_webview_event<R: Runtime>(webview: &tauri::Webview<R>, event: &tauri::WebviewEvent) {
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
        // The UI needs only where a drag starts and ends; Tauri may add
        // variants (the enum is non-exhaustive), which need nothing either.
        tauri::DragDropEvent::Over { .. } | _ => {}
    }
}

/// The app's configuration, assets and access control, as built into it;
/// the tests take the access control for their mock runtime.
#[expect(
    clippy::exit,
    clippy::disallowed_methods,
    reason = "tauri::generate_context! expands to process::exit for invalid embedded assets"
)]
fn context<R: Runtime>() -> tauri::Context<R> {
    tauri::generate_context!()
}

/// The commands and the media scheme, shared with the tests' mock runtime so
/// they exercise exactly what the webview can reach.
fn wire<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    // The tests also register a command `commands/names.rs` does not list,
    // to see the access control refuse it.
    let builder = builder
        .register_asynchronous_uri_scheme_protocol(media_scheme::SCHEME, media_scheme::handle);
    #[cfg(not(test))]
    let builder = builder.invoke_handler(app_commands!());
    #[cfg(test)]
    let builder = builder.invoke_handler(app_commands!(tests::unlisted_command,));
    builder
}

/// The handler of every app command (named in `commands/names.rs`), plus
/// the `extra` ones given.
macro_rules! app_commands {
    ($($extra:tt)*) => {
        tauri::generate_handler![
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
            settings::reconnect,
            settings::trust_server_key,
            link::create_link,
            link::join_link,
            devices::device_name,
            devices::link_start,
            devices::link_finish,
            devices::link_cancel,
            devices::link_device,
            devices::unlink_device,
            devices::replace_device,
            devices::cancel_history,
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
            push::push_enable,
            push::push_subscribe,
            push::push_disable,
            shell::set_tray,
            shell::set_close_to_tray,
            $($extra)*
        ]
    };
}
use app_commands;

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
