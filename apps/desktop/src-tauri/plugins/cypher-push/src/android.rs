use serde::{Deserialize, Serialize};
use tauri::Runtime;
use tauri::plugin::PluginHandle;

/// The Kotlin side (`PushPlugin`), managed as app state.
pub struct Push<R: Runtime>(pub(crate) PluginHandle<R>);

/// What the distributor answered: an endpoint and its Web Push keys
/// (base64url, unpadded), or why there is none.
#[derive(Debug, Deserialize)]
pub struct Subscribed {
    /// `ok`, `no_distributor` or `failed`.
    pub status: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub p256dh: String,
    #[serde(default)]
    pub auth: String,
}

#[derive(Serialize)]
struct SubscribeArgs<'a> {
    vapid: &'a str,
}

#[derive(Deserialize)]
struct Done {}

impl<R: Runtime> Push<R> {
    /// Registers with the phone's distributor for the server's VAPID key
    /// (base64url). Blocks until the distributor answers.
    ///
    /// # Errors
    /// The Kotlin side's message when registering cannot start.
    pub fn subscribe(&self, vapid: &str) -> Result<Subscribed, String> {
        self.0
            .run_mobile_plugin::<Subscribed>("subscribe", SubscribeArgs { vapid })
            .map_err(|e| e.to_string())
    }

    /// Unregisters from the distributor.
    ///
    /// # Errors
    /// The Kotlin side's message.
    pub fn unsubscribe(&self) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<Done>("unsubscribe", ())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}
