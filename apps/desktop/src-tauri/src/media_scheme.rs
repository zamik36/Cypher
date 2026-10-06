//! `cypher-media://<file_id>`: ranged playback of sealed voice and video
//! notes. Each request decrypts only the chunks covering the asked range,
//! so playback starts immediately and memory stays bounded. A note still
//! being received plays as far as it came; a request for bytes on their way
//! waits for them.
//!
//! `cypher-media://preview-<file_id>`: a picture received or sent as a file,
//! for its preview in the chat. Only images, only files this device keeps.

use std::time::Duration;

use cypher_client::{Client, ClientError, MediaSlice};
use cypher_types::FileId;
use tauri::http::{Request, Response, StatusCode, header};
use tauri::{AppHandle, Manager, Runtime, UriSchemeContext, UriSchemeResponder};

use crate::session::AppState;

pub(crate) const SCHEME: &str = "cypher-media";

#[expect(
    clippy::needless_pass_by_value,
    reason = "signature required by register_asynchronous_uri_scheme_protocol"
)]
pub(crate) fn handle<R: Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app = ctx.app_handle().clone();
    tauri::async_runtime::spawn(async move { responder.respond(answer(&app, &request).await) });
}

/// The response to `request`; 503 until a session is up.
pub(crate) async fn answer<R: Runtime>(
    app: &AppHandle<R>,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let Some(state) = app.try_state::<AppState>() else {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    };
    let Ok(client) = state.client().await else {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    };
    match request.uri().path().trim_matches('/').strip_prefix(PREVIEW) {
        Some(id) => preview(app, &client, id).await,
        None => serve(&client, request).await,
    }
}

const PREVIEW: &str = "preview-";
/// Larger pictures are not read whole for a preview (desktop).
const MAX_PREVIEW_BYTES: u64 = 20 << 20;

/// A kept picture, or 404 for anything else: an unknown id, a file that is
/// not an image, one too large or gone.
async fn preview<R: Runtime>(app: &AppHandle<R>, client: &Client, id: &str) -> Response<Vec<u8>> {
    let Some(file_id) = FileId::from_hex(id) else {
        return status(StatusCode::NOT_FOUND);
    };
    let Ok(Some(location)) = client.saved_file(file_id).await else {
        return status(StatusCode::NOT_FOUND);
    };
    match picture(app, location).await {
        Some((mime, bytes)) => Response::builder()
            .header(header::CONTENT_TYPE, mime)
            .header(header::CACHE_CONTROL, "no-store")
            .body(bytes)
            .unwrap_or_else(|_| status(StatusCode::INTERNAL_SERVER_ERROR)),
        None => status(StatusCode::NOT_FOUND),
    }
}

/// The picture at `location` with its type, if it is one.
async fn picture<R: Runtime>(_app: &AppHandle<R>, location: String) -> Option<(String, Vec<u8>)> {
    #[cfg(target_os = "android")]
    if location.starts_with("content://") {
        use tauri_plugin_cypher_files::Files;
        let app = _app.clone();
        let jpeg = tauri::async_runtime::spawn_blocking(move || {
            app.state::<Files<R>>().thumbnail(&location, THUMBNAIL_PX)
        })
        .await
        .ok()?
        .ok()?;
        return Some(("image/jpeg".to_owned(), jpeg));
    }
    let mime = mime_guess::from_path(&location).first()?;
    if mime.type_() != mime_guess::mime::IMAGE {
        return None;
    }
    let meta = tokio::fs::metadata(&location).await.ok()?;
    if meta.len() > MAX_PREVIEW_BYTES {
        return None;
    }
    let bytes = tokio::fs::read(&location).await.ok()?;
    Some((mime.essence_str().to_owned(), bytes))
}

/// Longest side of an Android thumbnail.
#[cfg(target_os = "android")]
const THUMBNAIL_PX: u32 = 720;

pub(crate) async fn serve(client: &Client, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let Some(file_id) = FileId::from_hex(request.uri().path().trim_matches('/')) else {
        return status(StatusCode::NOT_FOUND);
    };
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(parse_range);
    let result = match range {
        None => read_all(client, file_id).await.map(|s| (s, false)),
        Some(Some(Range::From(start, end))) => arrived(client, file_id, start, end)
            .await
            .map(|s| (s, true)),
        Some(Some(Range::Suffix(len))) => suffix(client, file_id, len).await.map(|s| (s, true)),
        Some(None) => return status(StatusCode::RANGE_NOT_SATISFIABLE),
    };
    match result {
        Ok((slice, partial)) => respond(slice, partial),
        Err(ClientError::InvalidInput) => status(StatusCode::RANGE_NOT_SATISFIABLE),
        Err(ClientError::NotReady) => status(StatusCode::SERVICE_UNAVAILABLE),
        Err(_) => status(StatusCode::NOT_FOUND),
    }
}

/// How long a request waits for bytes of a note still being received.
const ARRIVAL_WAIT: Duration = Duration::from_secs(30);
const ARRIVAL_POLL: Duration = Duration::from_millis(150);

/// `media_range`, waiting a while for a start that has not arrived yet.
async fn arrived(
    client: &Client,
    file_id: FileId,
    start: u64,
    end: Option<u64>,
) -> Result<MediaSlice, ClientError> {
    let deadline = tokio::time::Instant::now() + ARRIVAL_WAIT;
    loop {
        match client.media_range(file_id, start, end).await {
            Err(ClientError::NotReady) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(ARRIVAL_POLL).await;
            }
            result => return result,
        }
    }
}

async fn read_all(client: &Client, file_id: FileId) -> Result<MediaSlice, ClientError> {
    let mut slice = arrived(client, file_id, 0, None).await?;
    while slice.end + 1 < slice.total {
        let next = arrived(client, file_id, slice.end + 1, None).await?;
        slice.bytes.extend_from_slice(&next.bytes);
        slice.end = next.end;
    }
    Ok(slice)
}

async fn suffix(client: &Client, file_id: FileId, len: u64) -> Result<MediaSlice, ClientError> {
    let total = arrived(client, file_id, 0, Some(0)).await?.total;
    arrived(client, file_id, total.saturating_sub(len), None).await
}

fn respond(slice: MediaSlice, partial: bool) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, slice.mime.as_str())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_LENGTH, slice.bytes.len());
    builder = if partial {
        builder.status(StatusCode::PARTIAL_CONTENT).header(
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{}", slice.start, slice.end, slice.total),
        )
    } else {
        builder.status(StatusCode::OK)
    };
    builder
        .body(slice.bytes)
        .unwrap_or_else(|_| status(StatusCode::INTERNAL_SERVER_ERROR))
}

fn status(code: StatusCode) -> Response<Vec<u8>> {
    let mut response = Response::new(Vec::new());
    *response.status_mut() = code;
    response
}

#[derive(Debug, PartialEq, Eq)]
enum Range {
    /// `bytes=start-` or `bytes=start-end`.
    From(u64, Option<u64>),
    /// `bytes=-len`: the last `len` bytes.
    Suffix(u64),
}

/// Single-range `Range` header; multi-range requests are not supported.
fn parse_range(value: &str) -> Option<Range> {
    let spec = value.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    match (start.trim(), end.trim()) {
        ("", len) => len.parse().ok().filter(|&n| n > 0).map(Range::Suffix),
        (start, "") => start.parse().ok().map(|s| Range::From(s, None)),
        (start, end) => {
            let (s, e) = (start.parse().ok()?, end.parse().ok()?);
            (s <= e).then_some(Range::From(s, Some(e)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_byte_ranges() {
        assert_eq!(parse_range("bytes=0-"), Some(Range::From(0, None)));
        assert_eq!(parse_range("bytes=10-99"), Some(Range::From(10, Some(99))));
        assert_eq!(parse_range("bytes=-500"), Some(Range::Suffix(500)));
        assert_eq!(parse_range(" bytes=5-5 "), Some(Range::From(5, Some(5))));
    }

    #[test]
    fn rejects_malformed_and_multi_ranges() {
        for bad in [
            "bytes=9-3",
            "bytes=0-1,4-5",
            "items=0-1",
            "bytes=-0",
            "bytes=a-",
            "bytes=",
        ] {
            assert_eq!(parse_range(bad), None, "{bad}");
        }
    }
}
