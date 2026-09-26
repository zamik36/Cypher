use base64::Engine;
use image::Luma;
use qrcode::QrCode;

/// Generate a QR code PNG for a link_id, returned as a base64-encoded data URI.
#[tauri::command]
pub async fn generate_qr(link_id: String) -> Result<String, String> {
    let code = QrCode::new(link_id.as_bytes()).map_err(|e| e.to_string())?;
    let img = code.render::<Luma<u8>>().quiet_zone(true).build();

    let mut png_bytes = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png_bytes, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;

    let b64 = base64::engine::general_purpose::STANDARD.encode(png_bytes.get_ref());
    Ok(format!("data:image/png;base64,{}", b64))
}
