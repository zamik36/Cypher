//! Wake-ups while the app is closed, on Android through `UnifiedPush`. The
//! core asks the server for its key and registers the subscription through
//! the onion relay; the plugin gets the subscription from the phone's
//! distributor. Desktops stay connected in the tray and have no push.

use cypher_core::Command;
use tauri::{AppHandle, Runtime, State};

use crate::session::{AppState, CmdResult, err};

/// Asks the server for its push key; it arrives as `push_key`.
#[tauri::command]
pub(crate) async fn push_enable(state: State<'_, AppState>) -> CmdResult<()> {
    state
        .client()
        .await?
        .command(Command::EnablePush)
        .await
        .map_err(err)
}

/// Subscribes this phone with the server's key (hex) and registers the
/// subscription; `false` when there is no distributor or it refused.
#[tauri::command]
pub(crate) async fn push_subscribe<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    key: String,
) -> CmdResult<bool> {
    let Some((endpoint, p256dh, auth)) = subscribe(app, &key).await? else {
        return Ok(false);
    };
    let register = Command::RegisterPush {
        endpoint,
        p256dh,
        auth,
    };
    state.client().await?.command(register).await.map_err(err)?;
    Ok(true)
}

/// Unsubscribes this phone and has the server forget the subscription.
#[tauri::command]
pub(crate) async fn push_disable<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    unsubscribe(app).await?;
    state
        .client()
        .await?
        .command(Command::DisablePush)
        .await
        .map_err(err)
}

type Subscription = (String, [u8; 65], [u8; 16]);

#[cfg(target_os = "android")]
async fn subscribe<R: Runtime>(
    app: AppHandle<R>,
    key_hex: &str,
) -> CmdResult<Option<Subscription>> {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
    use tauri::Manager as _;
    use tauri_plugin_cypher_push::Push;

    let key = hex(key_hex).ok_or("invalid push key")?;
    let vapid = B64.encode(key);
    let answer =
        tauri::async_runtime::spawn_blocking(move || app.state::<Push<R>>().subscribe(&vapid))
            .await
            .map_err(err)??;
    if answer.status != "ok" {
        return Ok(None);
    }
    let p256dh = B64.decode(&answer.p256dh).map_err(err)?;
    let auth = B64.decode(&answer.auth).map_err(err)?;
    Ok(Some((
        answer.endpoint,
        p256dh.try_into().map_err(|_| "invalid push key")?,
        auth.try_into().map_err(|_| "invalid push key")?,
    )))
}

#[cfg(not(target_os = "android"))]
#[expect(clippy::unused_async, reason = "same shape as the Android version")]
async fn subscribe<R: Runtime>(
    _app: AppHandle<R>,
    key_hex: &str,
) -> CmdResult<Option<Subscription>> {
    hex(key_hex).ok_or("invalid push key")?;
    Ok(None)
}

#[cfg(target_os = "android")]
async fn unsubscribe<R: Runtime>(app: AppHandle<R>) -> CmdResult<()> {
    use tauri::Manager as _;
    use tauri_plugin_cypher_push::Push;

    tauri::async_runtime::spawn_blocking(move || app.state::<Push<R>>().unsubscribe())
        .await
        .map_err(err)?
        .map_err(Into::into)
}

#[cfg(not(target_os = "android"))]
#[expect(clippy::unused_async, reason = "same shape as the Android version")]
async fn unsubscribe<R: Runtime>(_app: AppHandle<R>) -> CmdResult<()> {
    Ok(())
}

/// The 65-byte key from its hex form.
fn hex(text: &str) -> Option<[u8; 65]> {
    if text.len() != 130 {
        return None;
    }
    let mut out = [0u8; 65];
    for (byte, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::hex;

    #[test]
    fn keys_come_as_hex() {
        let rest = "ab".repeat(64);
        let key = hex(&format!("04{rest}")).unwrap();
        assert_eq!((key[0], key[64]), (4, 0xab));
        assert!(hex(&rest).is_none());
        assert!(hex(&format!("zz{rest}")).is_none());
    }
}
