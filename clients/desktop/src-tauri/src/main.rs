// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// NOTE: This crate is NOT part of the Cargo workspace at the repo root.
// It is built exclusively by Tauri's toolchain (`cargo tauri dev` / `cargo tauri build`).
// Do not add `clients/desktop/src-tauri` to the root Cargo.toml workspace members.

fn main() {
    desktop_lib::run()
}
