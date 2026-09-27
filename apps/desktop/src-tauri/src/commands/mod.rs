//! Tauri commands. `#[tauri::command]` expands to code containing
//! `unreachable!`, which the strict lint set would otherwise reject.
#![expect(clippy::unreachable, reason = "expanded by #[tauri::command]")]

pub(crate) mod chat;
pub(crate) mod identity;
pub(crate) mod link;
pub(crate) mod media;
pub(crate) mod qr;
pub(crate) mod settings;
pub(crate) mod transfer;
