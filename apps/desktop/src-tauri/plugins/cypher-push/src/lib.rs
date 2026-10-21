//! Push on Android through `UnifiedPush`: the phone's distributor (ntfy, for
//! example) gives a Web Push endpoint for the server to signal, and the
//! plugin shows "New message" when a signal arrives, without starting the
//! app. Elsewhere the plugin does nothing.

use tauri::Runtime;
use tauri::plugin::{Builder, TauriPlugin};

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::{Error, Push, Subscribed};

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("cypher-push")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                let handle = api.register_android_plugin("app.cypher.push", "PushPlugin")?;
                app.manage(Push(handle));
            }
            #[cfg(not(target_os = "android"))]
            let _ = (app, api);
            Ok(())
        })
        .build()
}
