use serde::{Deserialize, Serialize};
use tauri::Runtime;
use tauri::plugin::PluginHandle;

/// The Kotlin side (`FilesPlugin`), managed as app state.
pub struct Files<R: Runtime>(pub(crate) PluginHandle<R>);

/// A picked file copied into the app for sending.
#[derive(Debug, Deserialize)]
pub struct Staged {
    pub path: String,
    pub name: String,
    pub mime: String,
}

#[derive(Serialize)]
struct PublishArgs<'a> {
    path: &'a str,
    name: &'a str,
    mime: &'a str,
}

#[derive(Serialize)]
struct UriArgs<'a> {
    uri: &'a str,
}

#[derive(Deserialize)]
struct Published {
    uri: String,
}

#[derive(Serialize)]
struct ThumbnailArgs<'a> {
    uri: &'a str,
    size: u32,
}

#[derive(Deserialize)]
struct Thumbnail {
    /// JPEG, base64.
    jpeg: String,
}

impl<R: Runtime> Files<R> {
    /// Moves a received file into Downloads/Cypher; returns its `content://`
    /// URI. The file at `path` is gone afterwards.
    ///
    /// # Errors
    /// The Kotlin side's message when the file cannot be published.
    pub fn publish(&self, path: &str, name: &str, mime: &str) -> Result<String, String> {
        self.0
            .run_mobile_plugin::<Published>("publish", PublishArgs { path, name, mime })
            .map(|p| p.uri)
            .map_err(|e| e.to_string())
    }

    /// Opens a `content://` URI with the app the user picks.
    ///
    /// # Errors
    /// `"no_app"` when nothing on the phone opens this kind of file.
    pub fn open(&self, uri: &str) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<()>("open", UriArgs { uri })
            .map_err(|e| e.to_string())
    }

    /// A JPEG of the picture at `uri`, its longest side at most `size`.
    ///
    /// # Errors
    /// The Kotlin side's message when it is not a picture or cannot be read.
    pub fn thumbnail(&self, uri: &str, size: u32) -> Result<Vec<u8>, String> {
        use base64::Engine as _;
        let thumb: Thumbnail = self
            .0
            .run_mobile_plugin("thumbnail", ThumbnailArgs { uri, size })
            .map_err(|e| e.to_string())?;
        base64::engine::general_purpose::STANDARD
            .decode(thumb.jpeg)
            .map_err(|e| e.to_string())
    }

    /// Copies a picked `content://` file into the app, for sending.
    ///
    /// # Errors
    /// The Kotlin side's message when the content cannot be read.
    pub fn stage(&self, uri: &str) -> Result<Staged, String> {
        self.0
            .run_mobile_plugin::<Staged>("stage", UriArgs { uri })
            .map_err(|e| e.to_string())
    }
}
