use base64::Engine;
use image::Luma;
use qrcode::QrCode;

use crate::session::{CmdResult, err};

/// Renders `link_id` as a PNG data URI.
#[tauri::command]
pub(crate) async fn generate_qr(link_id: String) -> CmdResult<String> {
    let code = QrCode::new(link_id.as_bytes()).map_err(err)?;
    let img = code.render::<Luma<u8>>().quiet_zone(true).build();
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png)
        .map_err(err)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(png.get_ref());
    Ok(format!("data:image/png;base64,{b64}"))
}
