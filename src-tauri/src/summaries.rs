//! The IPC surface for summaries, models, and the API key.
//!
//! Everything here is a thin wrapper over `oh-inference`. The two things this module
//! adds are the window-facing shape of each call and the download bookkeeping: a
//! download runs in its own task, reports progress by event, and can be cancelled from
//! the settings page while it runs.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::NaiveDate;
use oh_core::{CloudModelChoice, Config, DaySummary, HourSummary, InferenceProvider};
use oh_inference::catalog::{self, ModelStatus};
use oh_inference::download::{Cancel, Progress, ProgressListener};
use oh_inference::llama::LlamaStatus;
use oh_inference::prompt::ChatTurn;
use oh_inference::secrets::{self, SECRETS, Secret};
use oh_inference::service::{InferenceService, Readiness, RunReport};
use oh_processing::DayReport;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;

use crate::{AppState, parse_date, to_message};

/// Event name the settings page listens on while a model downloads.
pub const DOWNLOAD_EVENT: &str = "openhistory://download";

/// The inference service and the downloads currently in flight.
pub struct SummaryState {
    service: Arc<InferenceService>,
    downloads: Mutex<HashMap<String, Cancel>>,
}

impl SummaryState {
    pub fn new(service: Arc<InferenceService>) -> Self {
        SummaryState {
            service,
            downloads: Mutex::new(HashMap::new()),
        }
    }

    pub fn service(&self) -> Arc<InferenceService> {
        Arc::clone(&self.service)
    }
}

/// One step of a download, as the interface sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub model_id: String,
    pub downloaded_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    pub done: bool,
    /// Set when the download stopped without finishing. Cancelling counts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DownloadProgress {
    fn step(model_id: &str, progress: Progress) -> Self {
        DownloadProgress {
            model_id: model_id.to_owned(),
            downloaded_bytes: progress.downloaded_bytes,
            total_bytes: progress.total_bytes,
            done: progress.done,
            error: None,
        }
    }

    fn failed(model_id: &str, error: impl std::fmt::Display) -> Self {
        DownloadProgress {
            model_id: model_id.to_owned(),
            downloaded_bytes: 0,
            total_bytes: None,
            done: true,
            error: Some(error.to_string()),
        }
    }
}

/// One entry of the cloud dropdown, with what the settings page needs to draw it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudModelEntry {
    #[serde(flatten)]
    pub choice: CloudModelChoice,
    /// The company that runs it, for grouping the list.
    pub vendor: &'static str,
    /// A key for this provider is stored, so choosing this model would work.
    pub has_key: bool,
}

/// Every cloud model offered, in dropdown order.
///
/// One list rather than one per provider: the question being answered is "which model
/// writes my summaries", and the provider follows from the answer.
#[tauri::command]
pub fn cloud_models() -> Vec<CloudModelEntry> {
    oh_core::CLOUD_MODELS
        .iter()
        .map(|choice| CloudModelEntry {
            choice: *choice,
            vendor: choice.vendor(),
            has_key: Secret::for_provider(choice.provider).is_some_and(secrets::is_stored),
        })
        .collect()
}

/// Choose a cloud model. The provider is set from the catalog, not by the window.
#[tauri::command]
pub fn use_cloud_model(id: String, app: State<'_, AppState>) -> Result<Config, String> {
    let provider = oh_core::provider_for_model(&id)
        .ok_or_else(|| format!("{id} is not a model in the list"))?;

    let mut config = app.config();
    config.inference.provider = provider;
    config.inference.cloud_model = id;
    app.apply(config.clone())?;
    Ok(config)
}

/// Whether summaries can be produced with the current settings, and why not.
#[tauri::command]
pub fn inference_readiness(app: State<'_, AppState>, state: State<'_, SummaryState>) -> Readiness {
    state.service.readiness(&app.config())
}

/// The curated local models, with what is true of each on this machine.
#[tauri::command]
pub fn local_models() -> Result<Vec<ModelStatus>, String> {
    catalog::statuses().map_err(to_message)
}

/// Fetch a model, reporting progress on [`DOWNLOAD_EVENT`] as it goes.
///
/// Resolves when the download finishes, is cancelled, or fails. The interface does not
/// have to wait for it: the same outcome arrives as a final event.
#[tauri::command]
pub async fn download_model(
    id: String,
    handle: AppHandle,
    state: State<'_, SummaryState>,
) -> Result<ModelStatus, String> {
    let model = catalog::find(&id).ok_or_else(|| format!("{id} is not a model in the catalog"))?;
    let dir = oh_core::paths::models_dir().map_err(to_message)?;

    let cancel = Cancel::new();
    if state
        .downloads
        .lock()
        .insert(id.clone(), cancel.clone())
        .is_some()
    {
        return Err(format!("{id} is already downloading"));
    }

    let emitter = handle.clone();
    let model_id = id.clone();
    let listener: ProgressListener = Arc::new(move |progress| {
        let _ = emitter.emit(DOWNLOAD_EVENT, DownloadProgress::step(&model_id, progress));
    });

    let outcome = oh_inference::download::fetch_model(&model, &dir, Some(listener), &cancel).await;
    state.downloads.lock().remove(&id);

    match outcome {
        Ok(_) => {
            let _ = handle.emit(
                DOWNLOAD_EVENT,
                DownloadProgress {
                    model_id: id.clone(),
                    downloaded_bytes: model.approximate_bytes,
                    total_bytes: None,
                    done: true,
                    error: None,
                },
            );
            catalog::statuses()
                .map_err(to_message)?
                .into_iter()
                .find(|status| status.model.id == id)
                .ok_or_else(|| format!("{id} vanished from the catalog"))
        }
        Err(error) => {
            let _ = handle.emit(DOWNLOAD_EVENT, DownloadProgress::failed(&id, &error));
            Err(error.to_string())
        }
    }
}

/// Abandon a download in progress. The part that was fetched is kept for a resume.
#[tauri::command]
pub fn cancel_download(id: String, state: State<'_, SummaryState>) -> bool {
    match state.downloads.lock().get(&id) {
        Some(cancel) => {
            cancel.cancel();
            true
        }
        None => false,
    }
}

/// Delete a downloaded model file.
#[tauri::command]
pub fn remove_model(id: String, app: State<'_, AppState>) -> Result<bool, String> {
    let removed = catalog::remove(&id).map_err(to_message)?;

    // A model that is no longer on disk cannot stay selected, or every summary from
    // here on would fail with a missing file.
    let mut config = app.config();
    if config.inference.local_model_id.as_deref() == Some(id.as_str()) {
        config.inference.local_model_id = None;
        config.inference.local_model_path = None;
        app.apply(config)?;
    }
    Ok(removed)
}

/// Choose a downloaded model, resolving its path from the catalog.
#[tauri::command]
pub fn use_local_model(id: String, app: State<'_, AppState>) -> Result<Config, String> {
    let mut config = app.config();
    select_local_model(&mut config, &id)?;
    app.apply(config.clone())?;
    Ok(config)
}

/// Point the settings at a model on this machine.
///
/// Choosing the model also selects the local provider, exactly as choosing a hosted
/// model selects the vendor that runs it. Leaving the provider alone meant picking a
/// model here set the identifier and the path and went on sending every summary to
/// whichever cloud was selected before, while the settings page showed the local model
/// as chosen. Split out from the command so the pairing can be tested without a window.
pub fn select_local_model(config: &mut Config, id: &str) -> Result<(), String> {
    resolve_local_model(config, Some(id))?;
    config.inference.provider = InferenceProvider::Local;
    Ok(())
}

/// Ask the user where `llama-server` is, and remember the answer.
///
/// Returns the settings as they now stand, or `None` when the dialog was dismissed.
/// Nothing ships the binary, so on most machines it is neither beside the application
/// nor on `PATH`; without this a downloaded model cannot be used at all.
#[tauri::command]
pub fn choose_local_server(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<Config>, String> {
    let mut picker = app.dialog().file().set_title("Find llama-server");
    if cfg!(windows) {
        picker = picker.add_filter("Programs", &["exe"]);
    }
    let Some(chosen) = picker.blocking_pick_file() else {
        return Ok(None);
    };
    let path = chosen
        .into_path()
        .map_err(|error| format!("that file cannot be read: {error}"))?;
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }

    let mut config = state.config();
    config.inference.local_server_path = Some(path);
    state.apply(config.clone())?;
    Ok(Some(config))
}

/// Go back to looking beside the application and on `PATH`.
#[tauri::command]
pub fn forget_local_server(state: State<'_, AppState>) -> Result<Config, String> {
    let mut config = state.config();
    config.inference.local_server_path = None;
    state.apply(config.clone())?;
    Ok(config)
}

/// The identifier the `llama-server` fetch reports under on [`DOWNLOAD_EVENT`].
///
/// It rides the same event as the models so the settings page has one progress path
/// rather than two that drift apart.
pub const RUNTIME_ID: &str = "llama-server";

/// What the settings page knows about the program that runs local models.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    /// The llama.cpp build this application fetches.
    pub build: &'static str,
    /// A server is available, so a local model can actually run.
    pub installed: bool,
    /// Where it is, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<std::path::PathBuf>,
    /// The path came from the user rather than from the fetch.
    pub chosen: bool,
    /// How large the download is, before the server has said.
    pub approximate_bytes: u64,
}

impl ServerStatus {
    fn of(config: &Config) -> Self {
        let chosen = config
            .inference
            .local_server_path
            .as_ref()
            .filter(|path| path.is_file())
            .cloned();
        let path = chosen.clone().or_else(oh_inference::runtime::installed);
        ServerStatus {
            build: oh_inference::runtime::BUILD,
            installed: path.is_some(),
            path,
            chosen: chosen.is_some(),
            approximate_bytes: oh_inference::runtime::APPROXIMATE_BYTES,
        }
    }
}

/// Whether the local provider has a program to run, and which one.
#[tauri::command]
pub fn local_server(app: State<'_, AppState>) -> ServerStatus {
    ServerStatus::of(&app.config())
}

/// Fetch `llama-server`, reporting progress on [`DOWNLOAD_EVENT`] under [`RUNTIME_ID`].
///
/// Fetching one that is already there is a no-op, so the settings page can call this
/// whenever a local model is chosen without asking first whether it needs to.
#[tauri::command]
pub async fn fetch_local_server(
    handle: AppHandle,
    app: State<'_, AppState>,
    state: State<'_, SummaryState>,
) -> Result<ServerStatus, String> {
    let cancel = Cancel::new();
    if state
        .downloads
        .lock()
        .insert(RUNTIME_ID.to_owned(), cancel.clone())
        .is_some()
    {
        return Err("llama-server is already being fetched".to_owned());
    }

    let emitter = handle.clone();
    let listener: ProgressListener = Arc::new(move |progress| {
        let _ = emitter.emit(DOWNLOAD_EVENT, DownloadProgress::step(RUNTIME_ID, progress));
    });

    let outcome = oh_inference::runtime::fetch(Some(listener), &cancel).await;
    state.downloads.lock().remove(RUNTIME_ID);

    match outcome {
        Ok(_) => {
            // The last byte arriving is not the end of the work: the archive still has
            // to be unpacked. Announcing done only now keeps the interface from saying
            // it is ready while fifty DLLs are still being written.
            let _ = handle.emit(
                DOWNLOAD_EVENT,
                DownloadProgress {
                    model_id: RUNTIME_ID.to_owned(),
                    downloaded_bytes: oh_inference::runtime::APPROXIMATE_BYTES,
                    total_bytes: None,
                    done: true,
                    error: None,
                },
            );
            Ok(ServerStatus::of(&app.config()))
        }
        Err(error) => {
            let _ = handle.emit(DOWNLOAD_EVENT, DownloadProgress::failed(RUNTIME_ID, &error));
            Err(error.to_string())
        }
    }
}

/// Fill in `local_model_path` from `local_model_id`.
///
/// The window chooses a model by identifier; where the file lives is the backend's
/// business, and letting the window send a path would let it send any path.
pub fn resolve_local_model(config: &mut Config, id: Option<&str>) -> Result<(), String> {
    let id = match id {
        Some(id) => Some(id.to_owned()),
        None => config.inference.local_model_id.clone(),
    };
    let Some(id) = id else {
        config.inference.local_model_path = None;
        return Ok(());
    };

    let model = catalog::find(&id).ok_or_else(|| format!("{id} is not a model in the catalog"))?;
    let dir = oh_core::paths::models_dir().map_err(to_message)?;
    config.inference.local_model_id = Some(id);
    config.inference.local_model_path = Some(model.path_in(&dir));
    Ok(())
}

/// Which providers have a key stored, and what each key is called.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub provider: &'static str,
    pub label: &'static str,
    pub stored: bool,
}

/// Store a provider's API key in the Windows Credential Manager.
///
/// Returns whether a key is now stored: sending an empty value clears it.
#[tauri::command]
pub fn store_api_key(provider: InferenceProvider, key: String) -> Result<bool, String> {
    let secret = secret_for(provider)?;
    secrets::store(secret, &key).map_err(to_message)?;
    Ok(secrets::is_stored(secret))
}

/// Which keys are stored. A stored key itself is never sent back to the window.
#[tauri::command]
pub fn api_keys() -> Vec<KeyStatus> {
    SECRETS
        .iter()
        .map(|secret| KeyStatus {
            provider: secret.provider().as_str(),
            label: secret.label(),
            stored: secrets::is_stored(*secret),
        })
        .collect()
}

#[tauri::command]
pub fn forget_api_key(provider: InferenceProvider) -> Result<(), String> {
    secrets::forget(secret_for(provider)?).map_err(to_message)
}

fn secret_for(provider: InferenceProvider) -> Result<Secret, String> {
    Secret::for_provider(provider)
        .ok_or_else(|| format!("{} does not use an API key", provider.as_str()))
}

/// Whatever has been written about a day, in whatever state it is in.
#[tauri::command]
pub fn day_summary(date: String, state: State<'_, SummaryState>) -> Result<DaySummary, String> {
    Ok(state.service.summary(parse_date(&date)?))
}

/// Summarize every hour of a day that does not have one yet, then the day.
#[tauri::command]
pub async fn summarize_day(
    date: String,
    rewrite: Option<bool>,
    app: State<'_, AppState>,
    state: State<'_, SummaryState>,
) -> Result<RunReport, String> {
    let (config, report) = prepare(&date, &app)?;
    state
        .service
        .summarize_day(&config, &report, rewrite.unwrap_or(false))
        .await
        .map_err(to_message)
}

/// Rewrite one hour, whatever was there before.
#[tauri::command]
pub async fn summarize_hour(
    date: String,
    hour: u32,
    app: State<'_, AppState>,
    state: State<'_, SummaryState>,
) -> Result<HourSummary, String> {
    let (config, report) = prepare(&date, &app)?;
    state
        .service
        .summarize_hour(&config, &report, hour)
        .await
        .map_err(to_message)
}

/// One answer from the summariser, with the provenance the transcript shows beside it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReply {
    pub text: String,
    /// The model that answered, so an answer written by one model is not read as
    /// having come from the model now selected.
    pub model: String,
}

/// Ask the summariser about a day.
///
/// The transcript is passed in rather than held here. Nothing about the conversation
/// is written to disk: a summary is a document a person keeps, and a question they
/// asked about a Tuesday afternoon is not.
#[tauri::command]
pub async fn chat_about_day(
    date: String,
    question: String,
    turns: Vec<ChatTurn>,
    app: State<'_, AppState>,
    state: State<'_, SummaryState>,
) -> Result<ChatReply, String> {
    let (config, report) = prepare(&date, &app)?;
    let completion = state
        .service
        .chat(&config, &report, &turns, &question)
        .await
        .map_err(to_message)?;

    Ok(ChatReply {
        text: completion.text,
        model: completion.model,
    })
}

#[tauri::command]
pub fn forget_summary(date: String, state: State<'_, SummaryState>) -> Result<(), String> {
    state.service.forget(parse_date(&date)?).map_err(to_message)
}

#[tauri::command]
pub fn local_server_status(state: State<'_, SummaryState>) -> LlamaStatus {
    state.service.local_status()
}

/// Unload the local model now rather than waiting for the idle timer.
#[tauri::command]
pub async fn stop_local_server(state: State<'_, SummaryState>) -> Result<LlamaStatus, String> {
    let service = state.service();
    service.shutdown().await;
    Ok(service.local_status())
}

/// The settings and the day's measurements, with no lock held.
///
/// The processor is behind a mutex and summarizing awaits a network call, so the
/// report is taken out first and the guard dropped before anything is sent.
fn prepare(date: &str, app: &State<'_, AppState>) -> Result<(Config, DayReport), String> {
    let date: NaiveDate = parse_date(date)?;
    let config = app.config();
    let report = app.processor.lock().day(date).map_err(to_message)?;
    Ok((config, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect this guards against shipped: a config with `localModelId` and
    /// `localModelPath` both filled in and `provider` still naming a cloud vendor, so
    /// the summaries kept going to the cloud and the radio button would not move.
    #[test]
    fn choosing_a_model_on_this_machine_also_leaves_the_cloud() {
        let mut config = Config::default();
        config.inference.provider = InferenceProvider::Anthropic;

        select_local_model(&mut config, "gemma-4-e2b-qat").expect("a catalog model");

        assert_eq!(config.inference.provider, InferenceProvider::Local);
        assert_eq!(
            config.inference.local_model_id.as_deref(),
            Some("gemma-4-e2b-qat")
        );
        assert!(config.inference.local_model_path.is_some());
    }

    #[test]
    fn a_model_that_is_not_in_the_catalog_is_refused() {
        let mut config = Config::default();
        assert!(select_local_model(&mut config, "not-a-model").is_err());
        assert_eq!(config.inference.provider, InferenceProvider::Disabled);
    }
}
