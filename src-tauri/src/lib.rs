//! OpenHistory Tauri application.
//!
//! Service wiring lands in Phase 2. For now this boots a window and exposes a
//! version probe so the frontend has something real to call.

use serde::Serialize;

#[derive(Serialize)]
pub struct AppInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub phase: u8,
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        name: "OpenHistory",
        version: env!("CARGO_PKG_VERSION"),
        phase: 0,
    }
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,openhistory_win_lib=debug".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![app_info])
        .run(tauri::generate_context!())
        .expect("failed to start OpenHistory");
}
