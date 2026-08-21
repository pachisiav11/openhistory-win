//! OpenHistory Tauri application.
//!
//! Owns the collector service, the user's settings, the tray icon, and the IPC
//! surface the interface calls. All of the real work lives in the workspace crates;
//! this module is the wiring between them and the window.

pub mod collector_service;

use std::sync::Arc;

use chrono::NaiveDate;
use oh_core::{ActivityEvent, Config, EventStore, paths};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

use collector_service::{CollectorService, Status, StatusListener};

/// Event name the interface listens on for live status.
const STATUS_EVENT: &str = "openhistory://status";

pub struct AppState {
    service: Arc<CollectorService>,
    config: Mutex<Config>,
    /// The tray's recording checkbox, kept so it can follow the real state when
    /// recording is toggled from the window rather than from the tray.
    recording_item: Mutex<Option<CheckMenuItem<tauri::Wry>>>,
}

impl AppState {
    fn config(&self) -> Config {
        self.config.lock().clone()
    }

    /// Persist settings and make the tray agree with them.
    fn apply(&self, config: Config) -> Result<(), String> {
        config.save().map_err(to_message)?;
        if let Some(item) = self.recording_item.lock().as_ref() {
            let _ = item.set_checked(config.recording_enabled);
        }
        *self.config.lock() = config;
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub phase: u8,
    pub data_dir: String,
}

fn to_message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn parse_date(date: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| format!("{date} is not a date in YYYY-MM-DD form"))
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        name: "OpenHistory",
        version: env!("CARGO_PKG_VERSION"),
        phase: 2,
        data_dir: paths::data_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
    }
}

#[tauri::command]
fn get_status(state: State<'_, AppState>) -> Status {
    state.service.status()
}

#[tauri::command]
fn start_collector(state: State<'_, AppState>) -> Result<Status, String> {
    let mut config = state.config();
    if !config.recording_enabled {
        config.recording_enabled = true;
        state.apply(config.clone())?;
    }
    state.service.start(&config).map_err(to_message)?;
    Ok(state.service.status())
}

#[tauri::command]
fn stop_collector(state: State<'_, AppState>) -> Status {
    let mut config = state.config();
    if config.recording_enabled {
        config.recording_enabled = false;
        let _ = state.apply(config);
    }
    state.service.stop();
    state.service.status()
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> Config {
    state.config()
}

/// Replace the settings wholesale and restart the collector if it is running.
#[tauri::command]
fn set_config(config: Config, state: State<'_, AppState>) -> Result<Config, String> {
    state.apply(config.clone())?;
    state.service.reconfigure(&config).map_err(to_message)?;

    // `reconfigure` only restarts what was already running. Honour a setting that was
    // switched on while the collector was stopped.
    if config.recording_enabled && !state.service.is_running() {
        state.service.start(&config).map_err(to_message)?;
    }
    Ok(config)
}

/// Every event recorded on one local day.
#[tauri::command]
fn read_day(date: String) -> Result<Vec<ActivityEvent>, String> {
    oh_core::read_day(parse_date(&date)?).map_err(to_message)
}

/// Local dates that have any recorded history, oldest first.
#[tauri::command]
fn recorded_days() -> Result<Vec<String>, String> {
    let store = EventStore::open().map_err(to_message)?;
    Ok(store
        .recorded_days()
        .map_err(to_message)?
        .into_iter()
        .map(|date| date.format("%Y-%m-%d").to_string())
        .collect())
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &AppHandle, recording: bool) -> tauri::Result<CheckMenuItem<tauri::Wry>> {
    let open = MenuItem::with_id(app, "open", "Open OpenHistory", true, None::<&str>)?;
    let record = CheckMenuItem::with_id(app, "record", "Recording", true, recording, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &record, &separator, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().expect("bundled icon").clone())
        .tooltip("OpenHistory")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_window(app),
            "record" => toggle_recording(app),
            "quit" => {
                // Stop the collector explicitly. `exit` does not unwind, so the
                // service's own `Drop` would never flush the pending events.
                if let Some(state) = app.try_state::<AppState>() {
                    state.service.stop();
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(record)
}

fn toggle_recording(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };

    let mut config = state.config();
    config.recording_enabled = !config.recording_enabled;
    if let Err(error) = state.apply(config.clone()) {
        tracing::error!(%error, "could not save the recording setting");
    }

    let outcome = if config.recording_enabled {
        state.service.start(&config)
    } else {
        state.service.stop();
        Ok(())
    };
    if let Err(error) = outcome {
        tracing::error!(%error, "could not change the recording state");
    }
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,openhistory_win_lib=debug,oh_collector=debug".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            paths::ensure_layout()?;
            let config = Config::load().unwrap_or_default();

            // Write the defaults out on the first run. The file is the documented way
            // to see and hand-edit what the application is doing, so it should exist
            // before the user goes looking for it.
            if let Ok(path) = paths::config_file()
                && !path.exists()
                && let Err(error) = config.save()
            {
                tracing::warn!(%error, "could not write the initial settings file");
            }

            let handle = app.handle().clone();
            let listener: StatusListener = Arc::new(move |status: &Status| {
                let _ = handle.emit(STATUS_EVENT, status);
            });

            let service = Arc::new(CollectorService::new(listener));
            if config.recording_enabled && config.start_on_launch {
                if let Err(error) = service.start(&config) {
                    tracing::error!(%error, "could not start recording at launch");
                }
            }

            let recording_item = build_tray(app.handle(), config.recording_enabled)?;
            app.manage(AppState {
                service,
                config: Mutex::new(config),
                recording_item: Mutex::new(Some(recording_item)),
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window leaves the application recording in the tray. A
            // history that stops the moment you tidy your desktop is not a history.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            get_status,
            start_collector,
            stop_collector,
            get_config,
            set_config,
            read_day,
            recorded_days,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start OpenHistory");
}
