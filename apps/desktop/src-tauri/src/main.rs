#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    match desktop_lib::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("application failed: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
