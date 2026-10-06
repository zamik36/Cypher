//! The app around the window on desktops: a tray icon with Open and Quit,
//! closing to the tray, and one instance that receives invite links.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, Runtime, State};

use crate::session::CmdResult;

/// Whether closing the window hides it to the tray (desktop).
pub(crate) struct CloseToTray(pub AtomicBool);

impl Default for CloseToTray {
    fn default() -> Self {
        Self(AtomicBool::new(true))
    }
}

/// The tray menu's entries, so the UI can word them in its language.
#[cfg(desktop)]
pub(crate) struct TrayMenu<R: Runtime> {
    open: tauri::menu::MenuItem<R>,
    quit: tauri::menu::MenuItem<R>,
}

#[cfg(desktop)]
const TRAY_ID: &str = "main";

/// Brings the main window back, from the tray or behind other windows.
#[cfg(desktop)]
pub(crate) fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// The tray icon: a click shows the window; the menu opens or quits.
#[cfg(desktop)]
pub(crate) fn install_tray<R: Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let open = MenuItem::with_id(app, "open", "Шифр", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Шифр")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    app.manage(TrayMenu { open, quit });
    Ok(())
}

/// Hides the window instead of closing it when the user wants the app to
/// stay in the tray.
#[cfg(desktop)]
pub(crate) fn on_close<R: Runtime>(window: &tauri::Window<R>, event: &tauri::WindowEvent) {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event
        && window
            .app_handle()
            .try_state::<CloseToTray>()
            .is_some_and(|close| close.0.load(Ordering::Relaxed))
    {
        api.prevent_close();
        let _ = window.hide();
    }
}

/// Words the tray in the UI's language; `tooltip` carries the unread count.
#[expect(
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    reason = "#[tauri::command] takes owned arguments and returns a result"
)]
#[tauri::command]
pub(crate) fn set_tray<R: Runtime>(
    app: AppHandle<R>,
    open: String,
    quit: String,
    tooltip: String,
) -> CmdResult<()> {
    #[cfg(desktop)]
    {
        if let Some(menu) = app.try_state::<TrayMenu<R>>() {
            let _ = menu.open.set_text(open);
            let _ = menu.quit.set_text(quit);
        }
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            let _ = tray.set_tooltip(Some(tooltip));
        }
    }
    #[cfg(mobile)]
    let _ = (app, open, quit, tooltip);
    Ok(())
}

#[expect(
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    reason = "#[tauri::command] takes owned arguments and returns a result"
)]
#[tauri::command]
pub(crate) fn set_close_to_tray(close: State<'_, CloseToTray>, enabled: bool) -> CmdResult<()> {
    close.0.store(enabled, Ordering::Relaxed);
    Ok(())
}
