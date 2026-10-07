//! Push on Android through `UnifiedPush`: the phone's distributor (ntfy, for
//! example) gives a Web Push endpoint for the server to signal, and the
//! plugin shows "New message" when a signal arrives, without starting the
//! app. Elsewhere the plugin does nothing.

use tauri::Runtime;
use tauri::plugin::{Builder, TauriPlugin};

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::{Push, Subscribed};

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("cypher-push")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                let handle = _api.register_android_plugin("app.cypher.push", "PushPlugin")?;
                _app.manage(Push(handle));
            }
            Ok(())
        })
        .build()
}
