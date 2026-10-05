//! Shared storage on Android, where files live behind `content://` URIs:
//! received files go to Downloads, open in another app, and picked files
//! are copied out for sending. Elsewhere the plugin does nothing.

use tauri::Runtime;
use tauri::plugin::{Builder, TauriPlugin};

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::{Files, Staged};

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("cypher-files")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                let handle = _api.register_android_plugin("app.cypher.files", "FilesPlugin")?;
                _app.manage(Files(handle));
            }
            Ok(())
        })
        .build()
}
