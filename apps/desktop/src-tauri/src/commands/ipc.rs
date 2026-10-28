//! Binary bodies over IPC: raw bytes from desktop webviews, `{"data":
//! "<base64>"}` from Android, whose bridge carries only JSON.

use std::borrow::Cow;

use base64::Engine as _;
use tauri::ipc::InvokeBody;

use cypher_core::{CmdError, ErrorKind};

use crate::session::CmdResult;

/// The body's bytes, at most `max` of them; `what` names them in errors.
pub(crate) fn bytes<'a>(body: &'a InvokeBody, max: usize, what: &str) -> CmdResult<Cow<'a, [u8]>> {
    match body {
        InvokeBody::Raw(bytes) if bytes.len() > max => Err(too_large(what)),
        InvokeBody::Raw(bytes) => Ok(Cow::Borrowed(bytes)),
        InvokeBody::Json(value) => {
            let data = value
                .get("data")
                .and_then(|data| data.as_str())
                .ok_or_else(|| {
                    invalid(format!("expected the {what} as raw bytes or base64 data"))
                })?;
            if data.len() > max.div_ceil(3) * 4 {
                return Err(too_large(what));
            }
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .map(Cow::Owned)
                .map_err(|_| invalid(format!("the {what} data is not valid base64")))
        }
    }
}

/// A header's value, percent-decoded (names may hold any Unicode).
pub(crate) fn header(headers: &tauri::http::HeaderMap, name: &str) -> CmdResult<String> {
    let raw = headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| invalid(format!("missing {name} header")))?;
    percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .map(Cow::into_owned)
        .map_err(|_| invalid(format!("{name} is not UTF-8")))
}

fn invalid(message: String) -> CmdError {
    CmdError::new(ErrorKind::Invalid, message)
}

fn too_large(what: &str) -> CmdError {
    CmdError::new(ErrorKind::TooLarge, format!("{what} is too large"))
}
