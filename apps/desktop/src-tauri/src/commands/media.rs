//! Voice notes are recorded natively (cpal → Opus → WebM); video notes are
//! recorded by the webview's MediaRecorder and handed over as one raw IPC
//! body. Both are then sent, stored sealed and played through
//! `cypher-media://`.

use cypher_core::MediaKind;
use cypher_core::envelope::{MAX_MIME_LEN, MAX_POSTER_LEN};
use cypher_media::Recorder;
use serde::Serialize;
use tauri::ipc::{InvokeBody, Request};
use tauri::{AppHandle, Emitter, State};

use super::chat::parse_peer;
use crate::session::{AppState, CmdResult, err};

/// Shorter presses are treated as accidental taps and discarded.
const MIN_VOICE_MS: u32 = 500;
/// A 60 s round video at the recorder's bitrate is well under this.
const MAX_VIDEO_NOTE_BYTES: usize = 32 << 20;

#[derive(Serialize)]
pub struct MediaSent {
    msg_id: String,
    file_id: String,
    duration_ms: u32,
    waveform: Option<Vec<u8>>,
}

#[tauri::command]
pub async fn voice_start(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
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
pub async fn voice_stop(
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
pub async fn voice_cancel(state: State<'_, AppState>) -> CmdResult<()> {
    let recorder = state.voice.lock().map_err(err)?.take();
    if let Some(recorder) = recorder {
        tauri::async_runtime::spawn_blocking(move || recorder.cancel())
            .await
            .map_err(err)?;
    }
    Ok(())
}

/// Body: `poster JPEG ‖ video`, split by `x-poster-len`; the peer, MIME type
/// and duration travel in headers so the video is never base64-encoded.
#[tauri::command]
pub async fn send_video_note(
    state: State<'_, AppState>,
    request: Request<'_>,
) -> CmdResult<MediaSent> {
    let InvokeBody::Raw(body) = request.body() else {
        return Err("expected a binary body".into());
    };
    let header = |name: &str| -> CmdResult<&str> {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| format!("missing {name}"))
    };
    let peer = parse_peer(header("x-peer")?)?;
    let duration_ms: u32 = header("x-duration-ms")?.parse().map_err(err)?;
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
    let name = if mime.contains("mp4") {
        "note.mp4"
    } else {
        "note.webm"
    };
    let kind = MediaKind::VideoNote {
        duration_ms,
        poster: poster.to_vec(),
    };
    let (msg_id, file_id) = state
        .client()
        .await?
        .send_media(peer, video.to_vec(), name, mime, kind)
        .await
        .map_err(err)?;
    Ok(MediaSent {
        msg_id: msg_id.to_hex(),
        file_id: file_id.to_hex(),
        duration_ms,
        waveform: None,
    })
}
