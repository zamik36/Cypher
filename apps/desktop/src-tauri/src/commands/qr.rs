use base64::Engine;
use cypher_core::CmdError;
use qrcode::QrCode;
use qrcode::render::svg;

use crate::session::CmdResult;

/// Renders `link_id` as an SVG data URI, drawn as the web client draws it.
#[tauri::command]
pub(crate) async fn generate_qr(link_id: String) -> CmdResult<String> {
    let code = QrCode::new(link_id.as_bytes()).map_err(CmdError::internal)?;
    let image = code
        .render::<svg::Color<'_>>()
        .min_dimensions(180, 180)
        .quiet_zone(true)
        .build();
    let b64 = base64::engine::general_purpose::STANDARD.encode(image);
    Ok(format!("data:image/svg+xml;base64,{b64}"))
}
