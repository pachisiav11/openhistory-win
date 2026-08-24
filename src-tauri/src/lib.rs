//! OpenHistory Tauri application.
//!
//! Owns the collector service, the user's settings, the tray icon, and the IPC
//! surface the interface calls. All of the real work lives in the workspace crates;
//! this module is the wiring between them and the window.

pub mod auto_summary;
pub mod collector_service;
pub mod library;
pub mod mcp;
pub mod startup;
pub mod summaries;

use std::sync::Arc;

use chrono::NaiveDate;
use oh_core::{ActivityEvent, Config, EventStore, SummaryStore, paths};
use oh_inference::service::InferenceService;
use oh_mcp::{History, TokenStore};
use oh_processing::{DayReport, Processor, SearchHit};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

use collector_service::{CollectorService, Status, StatusListener};
use mcp::McpState;
use summaries::SummaryState;

/// Event name the interface listens on for live status.
const STATUS_EVENT: &str = "openhistory://status";

pub struct AppState {
    service: Arc<CollectorService>,
    config: Mutex<Config>,
    /// Derived history: episodes, rollups, and the search index. Held open because
    /// the index lives in memory and search runs on every keystroke, and shared with
    /// the MCP server, which must read the same index rather than a second copy.
    pub(crate) processor: Arc<Mutex<Processor>>,
    /// The tray's recording checkbox, kept so it can follow the real state when
    /// recording is toggled from the window rather than from the tray.
    recording_item: Mutex<Option<CheckMenuItem<tauri::Wry>>>,
}

impl AppState {
    pub(crate) fn config(&self) -> Config {
        self.config.lock().clone()
    }

    /// Persist settings and make the tray agree with them.
    pub(crate) fn apply(&self, config: Config) -> Result<(), String> {
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

pub(crate) fn to_message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

pub(crate) fn parse_date(date: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| format!("{date} is not a date in YYYY-MM-DD form"))
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        name: "OpenHistory",
        version: env!("CARGO_PKG_VERSION"),
        phase: 5,
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

/// Replace the settings wholesale and bring the collector and server into line.
#[tauri::command]
async fn set_config(
    mut config: Config,
    state: State<'_, AppState>,
    server: State<'_, McpState>,
) -> Result<Config, String> {
    // The window sends a model identifier; the path it lives at is resolved here so a
    // window can never point the local provider at an arbitrary file.
    summaries::resolve_local_model(&mut config, None)?;

    // Before the file is written, so a refused registry write leaves the settings
    // saying what is actually true rather than what was asked for.
    startup::apply(config.start_with_windows).map_err(to_message)?;

    state.apply(config.clone())?;
    state.service.reconfigure(&config).map_err(to_message)?;

    // `reconfigure` only restarts what was already running. Honour a setting that was
    // switched on while the collector was stopped.
    if config.recording_enabled && !state.service.is_running() {
        state.service.start(&config).map_err(to_message)?;
    }

    server.reconcile(&config.mcp).await?;
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

/// Episodes and measurements for one local day.
///
/// The day is processed on the way out if the event log has moved since the report
/// was last written, so the interface never has to ask for a refresh.
#[tauri::command]
fn day_report(date: String, state: State<'_, AppState>) -> Result<DayReport, String> {
    let date = parse_date(&date)?;
    state.processor.lock().day(date).map_err(to_message)
}

/// Episodes matching every term in the query, most recent first.
#[tauri::command]
fn search_history(
    query: String,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Vec<SearchHit> {
    state.processor.lock().search(&query, limit.unwrap_or(50))
}

/// Discard everything derived and rebuild it from the event log.
#[tauri::command]
fn rebuild_history(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let days = state.processor.lock().rebuild().map_err(to_message)?;
    Ok(days
        .into_iter()
        .map(|date| date.format("%Y-%m-%d").to_string())
        .collect())
}

/// What was thrown away, so the interface can say so rather than just claiming success.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Deleted {
    pub days: usize,
    pub summaries: usize,
}

/// Delete the event log, every report, the index, and every summary.
///
/// The collector is stopped first and restarted after: Windows will not delete a file
/// this process still holds open, and today's log is exactly that file. Recording
/// resumes if it was on, because "delete my history" is not "stop recording".
#[tauri::command]
fn delete_all_history(
    state: State<'_, AppState>,
    summaries: State<'_, SummaryState>,
) -> Result<Deleted, String> {
    let config = state.config();
    let was_running = state.service.is_running();
    state.service.stop();

    let outcome = (|| -> Result<Deleted, String> {
        let days = EventStore::open()
            .map_err(to_message)?
            .delete_all()
            .map_err(to_message)?;
        state.processor.lock().forget_all().map_err(to_message)?;
        let summaries = summaries.service().store().clear().map_err(to_message)?;
        Ok(Deleted {
            days: days.len(),
            summaries,
        })
    })();

    if was_running && let Err(error) = state.service.start(&config) {
        tracing::error!(%error, "could not resume recording after deleting the history");
    }
    outcome
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
                // The local model server is a child process. Killing it here rather
                // than leaving it to `Drop`, which `exit` never runs.
                if let Some(state) = app.try_state::<SummaryState>() {
                    let service = state.service();
                    tauri::async_runtime::block_on(service.shutdown());
                }
                // Close the listener before the process goes, so a relaunch finds its
                // preferred port free rather than falling back to another one.
                if let Some(state) = app.try_state::<McpState>() {
                    let server = state.server();
                    tauri::async_runtime::block_on(server.stop());
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
            // The window is created hidden so that the copy Windows starts at sign-in
            // goes to the tray without a window appearing over whatever the user is
            // doing. Every other launch shows it, and shows it first: the work below
            // takes long enough to look like a failure to start.
            if !startup::launched_by_windows()
                && let Some(window) = app.get_webview_window("main")
            {
                let _ = window.show();
            }

            paths::ensure_layout()?;
            let config = Config::load().unwrap_or_default();

            // Windows is told what the setting says on every launch, so an entry
            // removed by hand or left by an older install is put right.
            if let Err(error) = startup::apply(config.start_with_windows) {
                tracing::warn!(%error, "could not update the sign-in entry");
            }

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

            let inference = Arc::new(InferenceService::open()?);
            app.manage(SummaryState::new(Arc::clone(&inference)));

            // One processor for the whole application. The MCP server reads through
            // the same one the window does, so both see one search index.
            let processor = Arc::new(Mutex::new(Processor::open()?));
            let history = History::new(Arc::clone(&processor), Arc::new(SummaryStore::open()?));
            app.manage(McpState::new(history, TokenStore::open()?));

            let recording_item = build_tray(app.handle(), config.recording_enabled)?;
            app.manage(AppState {
                service,
                config: Mutex::new(config.clone()),
                processor,
                recording_item: Mutex::new(Some(recording_item)),
            });

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Some(state) = handle.try_state::<McpState>() {
                    mcp::start_if_enabled(&state, &config).await;
                }
            });

            auto_summary::spawn(app.handle().clone());
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
            day_report,
            search_history,
            rebuild_history,
            delete_all_history,
            summaries::cloud_models,
            summaries::use_cloud_model,
            summaries::inference_readiness,
            summaries::local_models,
            summaries::download_model,
            summaries::cancel_download,
            summaries::remove_model,
            summaries::use_local_model,
            summaries::choose_local_server,
            summaries::forget_local_server,
            summaries::local_server,
            summaries::fetch_local_server,
            summaries::store_api_key,
            summaries::api_keys,
            summaries::forget_api_key,
            summaries::day_summary,
            summaries::summarize_day,
            summaries::summarize_hour,
            summaries::forget_summary,
            summaries::local_server_status,
            summaries::stop_local_server,
            mcp::mcp_status,
            mcp::start_mcp,
            mcp::stop_mcp,
            mcp::regenerate_mcp_token,
            mcp::forget_mcp_tokens,
            mcp::mcp_client_config,
            library::library_entries,
            library::library_document,
            library::library_save,
            library::library_delete,
            library::library_export,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start OpenHistory");
}
