fn main() {
    // No commands for the webview: the app calls the plugin from Rust.
    tauri_plugin::Builder::new(&[])
        .android_path("android")
        .build();
}
