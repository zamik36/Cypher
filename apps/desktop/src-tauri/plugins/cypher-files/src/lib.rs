//! Shared storage on Android, where files live behind `content://` URIs:
//! received files go to Downloads, open in another app, and picked files
//! are copied out for sending. Elsewhere the plugin does nothing.

use tauri::Runtime;
use tauri::plugin::{Builder, TauriPlugin};

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::{Error, Files, Staged};

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("cypher-files")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                let handle = api.register_android_plugin("app.cypher.files", "FilesPlugin")?;
                app.manage(Files(handle));
            }
            #[cfg(not(target_os = "android"))]
            let _ = (app, api);
            Ok(())
        })
        .build()
}
