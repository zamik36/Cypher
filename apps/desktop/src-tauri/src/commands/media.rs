//! Voice notes are recorded natively (cpal → Opus → `WebM`); video notes are
//! recorded by the webview's `MediaRecorder` and handed over as one IPC body:
//! raw bytes on desktop, base64 in JSON on Android (whose webview cannot send
//! a raw body). Both are then sent, stored sealed and played through
//! `cypher-media://`.

use std::borrow::Cow;

use cypher_core::MediaKind;
use cypher_core::envelope::{MAX_MIME_LEN, MAX_POSTER_LEN};
use cypher_media::Recorder;
use cypher_types::PeerId;
use serde::Serialize;
use tauri::http::HeaderMap;
use tauri::ipc::{InvokeBody, Request};
use tauri::{AppHandle, Emitter, Runtime, State};

use super::chat::parse_peer;
use crate::session::{AppState, CmdResult, err};

/// Shorter presses are treated as accidental taps and discarded.
const MIN_VOICE_MS: u32 = 500;
/// A 60 s round video at the recorder's bitrate is well under this.
const MAX_VIDEO_NOTE_BYTES: usize = 32 << 20;

#[derive(Debug, Serialize)]
pub(crate) struct MediaSent {
    msg_id: String,
    file_id: String,
    duration_ms: u32,
    waveform: Option<Vec<u8>>,
}

#[tauri::command]
pub(crate) async fn voice_start<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    if state.voice.lock().map_err(err)?.is_some() {
        return Err("already recording".into());
    }
    let recorder = tauri::async_runtime::spawn_blocking(move || {
        Recorder::start(move |level| {
            let _ = app.emit("cypher://voice_level", level);
        })
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    let mut slot = state.voice.lock().map_err(err)?;
    if slot.is_some() {
        drop(slot);
        recorder.cancel();
        return Err("already recording".into());
    }
    *slot = Some(recorder);
    Ok(())
}

/// Stops recording and sends the note; `None` when it was too short.
#[tauri::command]
pub(crate) async fn voice_stop(
    state: State<'_, AppState>,
    peer_id: String,
) -> CmdResult<Option<MediaSent>> {
    let peer = parse_peer(&peer_id)?;
    let recorder = state
        .voice
        .lock()
        .map_err(err)?
        .take()
        .ok_or("not recording")?;
    let recording = tauri::async_runtime::spawn_blocking(move || recorder.finish())
        .await
        .map_err(err)?
        .map_err(err)?;
    if recording.duration_ms < MIN_VOICE_MS {
        return Ok(None);
    }
    let duration_ms = recording.duration_ms;
    let waveform = recording.waveform;
    let kind = MediaKind::Voice {
        duration_ms,
        waveform: waveform.clone(),
    };
    let (msg_id, file_id) = state
        .client()
        .await?
        .send_media(peer, recording.webm, "voice.webm", "audio/webm", kind)
        .await
        .map_err(err)?;
    Ok(Some(MediaSent {
        msg_id: msg_id.to_hex(),
        file_id: file_id.to_hex(),
        duration_ms,
        waveform: Some(waveform),
    }))
}

#[tauri::command]
pub(crate) async fn voice_cancel(state: State<'_, AppState>) -> CmdResult<()> {
    let recorder = state.voice.lock().map_err(err)?.take();
    if let Some(recorder) = recorder {
        tauri::async_runtime::spawn_blocking(move || recorder.cancel())
            .await
            .map_err(err)?;
    }
    Ok(())
}

/// A video note as the webview sends it: `poster JPEG ‖ video` in one raw
/// body split by `x-poster-len`; peer, MIME type and duration in headers, so
/// the video is never base64-encoded.
#[derive(Debug)]
struct VideoNote<'a> {
    peer: PeerId,
    duration_ms: u32,
    mime: &'a str,
    poster: &'a [u8],
    video: &'a [u8],
}

impl<'a> VideoNote<'a> {
    fn parse(headers: &'a HeaderMap, body: &'a [u8]) -> CmdResult<Self> {
        let header = |name: &str| -> CmdResult<&'a str> {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| format!("missing {name}"))
        };
        let peer = parse_peer(header("x-peer")?)?;
        let duration_ms = header("x-duration-ms")?.parse().map_err(err)?;
        let poster_len: usize = header("x-poster-len")?.parse().map_err(err)?;
        let mime = header("x-mime")?;
        if !mime.starts_with("video/") || mime.len() > MAX_MIME_LEN {
            return Err("unsupported video type".into());
        }
        if poster_len > MAX_POSTER_LEN
            || poster_len >= body.len()
            || body.len() - poster_len > MAX_VIDEO_NOTE_BYTES
        {
            return Err("video note is too large".into());
        }
        let (poster, video) = body.split_at(poster_len);
        Ok(Self {
            peer,
            duration_ms,
            mime,
            poster,
            video,
        })
    }

    fn file_name(&self) -> &'static str {
        if self.mime.contains("mp4") {
            "note.mp4"
        } else {
            "note.webm"
        }
    }
}

/// The note's bytes: raw from desktop webviews, base64 from Android.
fn note_body(body: &InvokeBody) -> CmdResult<Cow<'_, [u8]>> {
    super::ipc::bytes(body, MAX_VIDEO_NOTE_BYTES + MAX_POSTER_LEN, "video note")
}

#[tauri::command]
pub(crate) async fn send_video_note(
    state: State<'_, AppState>,
    request: Request<'_>,
) -> CmdResult<MediaSent> {
    let body = note_body(request.body())?;
    let note = VideoNote::parse(request.headers(), &body)?;
    let kind = MediaKind::VideoNote {
        duration_ms: note.duration_ms,
        poster: note.poster.to_vec(),
    };
    let (msg_id, file_id) = state
        .client()
        .await?
        .send_media(
            note.peer,
            note.video.to_vec(),
            note.file_name(),
            note.mime,
            kind,
        )
        .await
        .map_err(err)?;
    Ok(MediaSent {
        msg_id: msg_id.to_hex(),
        file_id: file_id.to_hex(),
        duration_ms: note.duration_ms,
        waveform: None,
    })
}

#[cfg(test)]
mod tests {
    use tauri::http::HeaderValue;

    use super::*;

    type Pairs = Vec<(&'static str, String)>;
    /// Header tweak, body, expected error fragment.
    type Case = (fn(&mut Pairs), &'static [u8], &'static str);

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|&(k, v)| (k.parse().unwrap(), HeaderValue::from_str(v).unwrap()))
            .collect()
    }

    fn valid() -> Pairs {
        vec![
            ("x-peer", PeerId([7; 32]).to_hex()),
            ("x-duration-ms", "4000".into()),
            ("x-poster-len", "2".into()),
            ("x-mime", "video/mp4".into()),
        ]
    }

    fn parse_with(
        change: impl FnOnce(&mut Pairs),
        body: &[u8],
    ) -> CmdResult<(u32, Vec<u8>, Vec<u8>, &'static str)> {
        let mut pairs = valid();
        change(&mut pairs);
        let borrowed: Vec<(&'static str, &str)> =
            pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let map = headers(&borrowed);
        VideoNote::parse(&map, body).map(|n| {
            (
                n.duration_ms,
                n.poster.to_vec(),
                n.video.to_vec(),
                n.file_name(),
            )
        })
    }

    #[test]
    fn body_splits_into_poster_and_video() {
        let parsed = parse_with(|_| {}, b"PPvideo").unwrap();
        assert_eq!(
            parsed,
            (4000, b"PP".to_vec(), b"video".to_vec(), "note.mp4")
        );
        let webm = parse_with(|h| h[3].1 = "video/webm".into(), b"PPv").unwrap();
        assert_eq!(webm.3, "note.webm");
    }

    #[test]
    fn malformed_requests_are_rejected() {
        let cases: [Case; 6] = [
            (|h| drop(h.remove(0)), b"PPv", "missing x-peer"),
            (|h| h[0].1 = "zz".into(), b"PPv", "invalid peer id"),
            (
                |h| h[3].1 = "audio/ogg".into(),
                b"PPv",
                "unsupported video type",
            ),
            (|h| h[2].1 = "3".into(), b"PPv", "too large"),
            (|h| h[1].1 = "-1".into(), b"PPv", "invalid digit"),
            (|_| {}, b"PP", "too large"),
        ];
        for (change, body, expected) in cases {
            let error = parse_with(change, body).unwrap_err();
            assert!(
                error.contains(expected),
                "{error} should mention {expected}"
            );
        }
    }
}
