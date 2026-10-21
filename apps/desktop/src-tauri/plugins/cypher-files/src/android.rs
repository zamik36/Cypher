use serde::{Deserialize, Serialize};
use tauri::Runtime;
use tauri::plugin::PluginHandle;
use tauri::plugin::mobile::PluginInvokeError;

/// Why a call to the Kotlin side failed. Its message is the string these
/// calls have always failed with (`no_app`, for example).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The Kotlin side refused the call or could not be reached.
    #[error(transparent)]
    Plugin(#[from] PluginInvokeError),
    /// The thumbnail the Kotlin side sent was not base64.
    #[error(transparent)]
    Thumbnail(base64::DecodeError),
}

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
    pub fn publish(&self, path: &str, name: &str, mime: &str) -> Result<String, Error> {
        let published = self
            .0
            .run_mobile_plugin::<Published>("publish", PublishArgs { path, name, mime })?;
        Ok(published.uri)
    }

    /// Opens a `content://` URI with the app the user picks.
    ///
    /// # Errors
    /// `"no_app"` when nothing on the phone opens this kind of file.
    pub fn open(&self, uri: &str) -> Result<(), Error> {
        Ok(self.0.run_mobile_plugin::<()>("open", UriArgs { uri })?)
    }

    /// A JPEG of the picture at `uri`, its longest side at most `size`.
    ///
    /// # Errors
    /// The Kotlin side's message when it is not a picture or cannot be read.
    pub fn thumbnail(&self, uri: &str, size: u32) -> Result<Vec<u8>, Error> {
        use base64::Engine as _;
        let thumb: Thumbnail = self
            .0
            .run_mobile_plugin("thumbnail", ThumbnailArgs { uri, size })?;
        base64::engine::general_purpose::STANDARD
            .decode(thumb.jpeg)
            .map_err(Error::Thumbnail)
    }

    /// Copies a picked `content://` file into the app, for sending.
    ///
    /// # Errors
    /// The Kotlin side's message when the content cannot be read.
    pub fn stage(&self, uri: &str) -> Result<Staged, Error> {
        Ok(self
            .0
            .run_mobile_plugin::<Staged>("stage", UriArgs { uri })?)
    }
}
