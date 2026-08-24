//! User settings, persisted to `%APPDATA%\openhistory-win\config.json`.
//!
//! Every field carries a `serde` default, so a config file written by an older build
//! loads without complaint and picks up new settings at their defaults. That is the
//! only compatibility rule this file has to honour.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

/// Applications excluded from recording out of the box.
///
/// Password managers are the clear case: their window titles routinely name the
/// account or site a credential belongs to, which is exactly the kind of detail this
/// application should never hold. The Windows credential and consent brokers are here
/// for the same reason.
pub const DEFAULT_EXCLUDED: &[&str] = &[
    "1password",
    "bitwarden",
    "keepass",
    "keepassxc",
    "lastpass",
    "dashlane",
    "enpass",
    "nordpass",
    "protonpass",
    "credentialuibroker",
    "consent",
    "lsass",
];

/// What the collector will and will not record.
///
/// This is the settings half of the collector's behaviour, kept in `oh-core` so the
/// application can persist it without depending on the collector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordingConfig {
    /// Executable file stems to ignore, compared case-insensitively.
    pub excluded_apps: Vec<String>,
    /// Record browser URLs at all. Turning this off still records which browser was
    /// in front, just not where the user went.
    pub capture_urls: bool,
    /// Record the document or file the foreground window is on: the name of the
    /// spreadsheet, not its contents.
    pub capture_documents: bool,
    /// Record a bounded amount of the text the window is displaying — tab labels,
    /// headings, the name of the thing being edited.
    ///
    /// This is the widest of the three and the reason the others are separate: a
    /// person can want to know which document they were in without wanting the words
    /// on the screen written down.
    pub capture_visible_text: bool,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        RecordingConfig {
            excluded_apps: DEFAULT_EXCLUDED.iter().map(|s| (*s).to_owned()).collect(),
            capture_urls: true,
            capture_documents: true,
            capture_visible_text: true,
        }
    }
}

impl RecordingConfig {
    /// True when an executable must not be recorded.
    pub fn excludes(&self, exe_stem: &str) -> bool {
        let lowered = exe_stem.to_ascii_lowercase();
        self.excluded_apps
            .iter()
            .any(|excluded| excluded.eq_ignore_ascii_case(&lowered))
    }

    pub fn exclude(&mut self, exe_stem: impl Into<String>) {
        let stem = exe_stem.into().to_ascii_lowercase();
        if !self.excluded_apps.contains(&stem) {
            self.excluded_apps.push(stem);
        }
    }

    pub fn allow(&mut self, exe_stem: &str) {
        let stem = exe_stem.to_ascii_lowercase();
        self.excluded_apps.retain(|excluded| *excluded != stem);
    }
}

/// Which engine writes the summaries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InferenceProvider {
    /// No summaries at all. The default: the application records and browses history
    /// without any model until the user chooses one.
    #[default]
    Disabled,
    /// The Anthropic Messages API.
    Anthropic,
    /// The OpenAI Responses API.
    #[serde(rename = "openai")]
    OpenAi,
    /// The Google AI Studio Gemini API.
    Google,
    /// A GGUF model run by a `llama-server` this application starts and stops.
    Local,
}

impl InferenceProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            InferenceProvider::Disabled => "disabled",
            InferenceProvider::Anthropic => "anthropic",
            InferenceProvider::OpenAi => "openai",
            InferenceProvider::Google => "google",
            InferenceProvider::Local => "local",
        }
    }

    /// The company's name, for a settings page that groups models by who runs them.
    pub fn vendor(self) -> &'static str {
        match self {
            InferenceProvider::Disabled => "None",
            InferenceProvider::Anthropic => "Anthropic",
            InferenceProvider::OpenAi => "OpenAI",
            InferenceProvider::Google => "Google AI Studio",
            InferenceProvider::Local => "This machine",
        }
    }

    /// True for the providers that send window titles and URLs off the machine.
    pub fn is_cloud(self) -> bool {
        matches!(
            self,
            InferenceProvider::Anthropic | InferenceProvider::OpenAi | InferenceProvider::Google
        )
    }
}

/// One entry of the cloud model dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudModelChoice {
    /// The identifier sent to the provider.
    pub id: &'static str,
    /// What the dropdown shows.
    pub name: &'static str,
    pub provider: InferenceProvider,
    pub note: &'static str,
    /// The model reasons before answering and accepts an effort setting. Summaries ask
    /// for the lowest effort available and leave room for the reasoning tokens, which
    /// every one of these providers counts against the output limit.
    pub supports_effort: bool,
}

impl CloudModelChoice {
    /// The company that runs it.
    pub fn vendor(&self) -> &'static str {
        self.provider.vendor()
    }
}

/// Every cloud model offered, as one list.
///
/// One dropdown rather than a provider choice followed by a model choice: the question
/// a person is actually answering is "which model writes my summaries", and the
/// provider follows from the answer. Only the current generation of each tier is
/// listed; `config.json` still accepts any identifier by hand, which is the escape
/// hatch for anyone who wants a specific snapshot.
pub const CLOUD_MODELS: &[CloudModelChoice] = &[
    CloudModelChoice {
        id: "claude-haiku-4-5",
        name: "Claude Haiku (latest)",
        provider: InferenceProvider::Anthropic,
        note: "Fastest and cheapest. Enough for a two-sentence summary of an hour.",
        supports_effort: false,
    },
    CloudModelChoice {
        id: "claude-sonnet-5",
        name: "Claude Sonnet (latest)",
        provider: InferenceProvider::Anthropic,
        note: "Better at spotting the thread through a scattered day.",
        supports_effort: true,
    },
    CloudModelChoice {
        id: "claude-opus-5",
        name: "Claude Opus (latest)",
        provider: InferenceProvider::Anthropic,
        note: "The most capable, and the most expensive per summary.",
        supports_effort: true,
    },
    CloudModelChoice {
        id: "gpt-5.6-luna",
        name: "GPT-5.6 Luna",
        provider: InferenceProvider::OpenAi,
        note: "The fastest and cheapest of the GPT-5.6 tiers.",
        supports_effort: true,
    },
    CloudModelChoice {
        id: "gpt-5.6-terra",
        name: "GPT-5.6 Terra",
        provider: InferenceProvider::OpenAi,
        note: "The balanced tier, at half the price of Sol.",
        supports_effort: true,
    },
    CloudModelChoice {
        id: "gpt-5.6-sol",
        name: "GPT-5.6 Sol",
        provider: InferenceProvider::OpenAi,
        note: "The flagship tier. More than a window-title summary needs.",
        supports_effort: true,
    },
    CloudModelChoice {
        // Google, unlike Anthropic, does publish moving aliases. This one follows
        // whatever the current Flash release is, which is the right trade for a
        // two-sentence summary: no maintenance here, and a two-week notice before
        // anything behind it changes in a breaking way.
        id: "gemini-flash-latest",
        name: "Gemini Flash (latest)",
        provider: InferenceProvider::Google,
        note: "Google's fast tier, through an AI Studio key.",
        supports_effort: false,
    },
];

/// The model used when the user has not chosen one.
///
/// Summarizing a handful of window titles is not a task that rewards a larger model,
/// and this is among the cheapest and fastest on the list.
pub const DEFAULT_CLOUD_MODEL: &str = "claude-haiku-4-5";

/// The catalog entry for a model identifier, when it is one of the seven offered.
///
/// Returns `None` for a model set by hand in `config.json`, which is treated as a
/// plain identifier with no special request shaping.
pub fn cloud_model(id: &str) -> Option<&'static CloudModelChoice> {
    CLOUD_MODELS.iter().find(|choice| choice.id == id)
}

/// Which provider serves a model identifier, when the list knows it.
pub fn provider_for_model(id: &str) -> Option<InferenceProvider> {
    cloud_model(id).map(|choice| choice.provider)
}

/// How long `llama-server` may sit idle before it is shut down, in seconds.
///
/// See AD-3: the model is not held resident. Five minutes is long enough that a run of
/// hourly summaries reuses one server, and short enough that an idle machine is not
/// holding gigabytes for nothing.
pub const DEFAULT_IDLE_UNLOAD_SECONDS: u64 = 300;

/// How summaries get written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InferenceConfig {
    pub provider: InferenceProvider,
    /// The user has been shown what leaves the machine and has agreed to it. Selecting
    /// the Anthropic provider is not on its own enough to start sending data.
    pub cloud_consent: bool,
    /// The cloud model identifier. Which provider it belongs to is looked up from
    /// [`CLOUD_MODELS`], so the window sets one field rather than two that can disagree.
    pub cloud_model: String,
    /// Catalog identifier of the downloaded model, when one was chosen from the list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_model_id: Option<String>,
    /// Full path to the GGUF file to load. Set for a catalog model and for a
    /// hand-picked one alike, so the runtime only ever has to read this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_model_path: Option<PathBuf>,
    /// Full path to `llama-server`, when the user has pointed at one.
    ///
    /// Nothing ships the binary, so on most machines it is neither beside the
    /// application nor on `PATH`, and a downloaded model is then unusable with no way
    /// to say where the server lives. This is that way. `None` keeps the search that
    /// was there before: beside the executable, then `PATH`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_server_path: Option<PathBuf>,
    /// Context window handed to `llama-server`.
    pub context_size: u32,
    pub idle_unload_seconds: u64,
    /// Write yesterday's day summary automatically each morning, rather than only
    /// when asked. Uses whichever provider is already chosen above.
    pub auto_summarize: bool,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        InferenceConfig {
            provider: InferenceProvider::Disabled,
            cloud_consent: false,
            cloud_model: DEFAULT_CLOUD_MODEL.to_owned(),
            local_model_id: None,
            local_model_path: None,
            local_server_path: None,
            context_size: 8192,
            idle_unload_seconds: DEFAULT_IDLE_UNLOAD_SECONDS,
            auto_summarize: false,
        }
    }
}

impl InferenceConfig {
    /// True when summaries can actually be produced with these settings.
    pub fn is_usable(&self) -> bool {
        match self.provider {
            InferenceProvider::Disabled => false,
            provider if provider.is_cloud() => self.cloud_consent,
            _ => self.local_model_path.is_some(),
        }
    }

    /// The chosen cloud model's catalog entry, when it is one of the offered ones.
    pub fn choice(&self) -> Option<&'static CloudModelChoice> {
        cloud_model(&self.cloud_model)
    }
}

/// The port the plan fixes for the local MCP server.
pub const DEFAULT_MCP_PORT: u16 = 47123;

/// The local server other tools read history through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct McpConfig {
    /// Off until the user turns it on. An open port that answers questions about what
    /// its owner has been doing is not something to start by default.
    pub enabled: bool,
    /// Preferred port. If it is taken, the server binds the next free one and reports
    /// where it actually landed.
    pub port: u16,
    /// Answer questions about days other than today. Turning this off narrows an
    /// enabled server to the current day, which is what an assistant helping with the
    /// work in front of you actually needs.
    pub allow_history: bool,
}

impl Default for McpConfig {
    fn default() -> Self {
        McpConfig {
            enabled: false,
            port: DEFAULT_MCP_PORT,
            allow_history: true,
        }
    }
}

/// Everything the user can change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Whether the collector runs. Turning this off stops recording without
    /// uninstalling anything or discarding existing history.
    pub recording_enabled: bool,
    /// Start collecting as soon as the application launches.
    pub start_on_launch: bool,
    /// Let Windows launch the application when the user signs in.
    ///
    /// On by default. A history with a hole in it every time the machine restarts is
    /// not a history, and the application is a tray program: launching it costs the
    /// user nothing they would notice.
    pub start_with_windows: bool,
    /// Days of history to keep. Zero means keep everything, which is the default: a
    /// personal history that silently deletes itself is not one you can rely on.
    pub retention_days: u32,
    pub recording: RecordingConfig,
    pub inference: InferenceConfig,
    pub mcp: McpConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            recording_enabled: true,
            start_on_launch: true,
            start_with_windows: true,
            retention_days: 0,
            recording: RecordingConfig::default(),
            inference: InferenceConfig::default(),
            mcp: McpConfig::default(),
        }
    }
}

impl Config {
    /// Load the real config file, or defaults if it does not exist yet.
    pub fn load() -> Result<Self> {
        Self::load_from(&paths::config_file()?)
    }

    /// Load from an explicit path.
    ///
    /// A file that cannot be parsed is moved aside to `<name>.bad` and defaults are
    /// returned. Refusing to start would be worse, and overwriting the file in place
    /// would destroy settings the user might still want to recover by hand.
    pub fn load_from(path: &Path) -> Result<Self> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Ok(Config::default());
        };

        match serde_json::from_str(&text) {
            Ok(config) => Ok(config),
            Err(error) => {
                let quarantine = path.with_extension("json.bad");
                tracing::error!(
                    %error,
                    moved_to = %quarantine.display(),
                    "config file could not be parsed; falling back to defaults"
                );
                let _ = std::fs::rename(path, &quarantine);
                Ok(Config::default())
            }
        }
    }

    /// Persist to the real config file.
    pub fn save(&self) -> Result<()> {
        self.save_to(&paths::config_file()?)
    }

    /// Persist to an explicit path.
    ///
    /// Writes a sibling temporary file and renames it over the target, so a crash
    /// mid-write leaves the previous settings intact rather than a half-written file.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            paths::ensure_dir(parent)?;
        }

        let text = serde_json::to_string_pretty(self).context("could not serialize settings")?;
        let temporary: PathBuf = path.with_extension("json.writing");
        std::fs::write(&temporary, text)
            .with_context(|| format!("could not write {}", temporary.display()))?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_managers_are_excluded_by_default() {
        let config = RecordingConfig::default();
        assert!(config.excludes("1Password"));
        assert!(config.excludes("bitwarden"));
        assert!(config.excludes("KeePassXC"));
        assert!(!config.excludes("Code"));
        assert!(!config.excludes("chrome"));
    }

    #[test]
    fn exclusions_are_case_insensitive_and_deduplicated() {
        let mut config = RecordingConfig {
            excluded_apps: Vec::new(),
            ..RecordingConfig::default()
        };
        config.exclude("Slack");
        config.exclude("SLACK");

        assert_eq!(config.excluded_apps, vec!["slack".to_string()]);
        assert!(config.excludes("slack"));
        assert!(config.excludes("Slack"));

        config.allow("SLACK");
        assert!(!config.excludes("slack"));
    }

    #[test]
    fn settings_survive_a_save_and_load() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");

        let mut config = Config {
            recording_enabled: false,
            retention_days: 90,
            ..Config::default()
        };
        config.recording.exclude("Signal");
        config.save_to(&path).unwrap();

        assert_eq!(Config::load_from(&path).unwrap(), config);
    }

    #[test]
    fn a_missing_file_loads_as_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        assert_eq!(Config::load_from(&path).unwrap(), Config::default());
    }

    #[test]
    fn an_older_file_gains_new_settings_at_their_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        std::fs::write(&path, r#"{"recordingEnabled": false}"#).unwrap();

        let config = Config::load_from(&path).unwrap();
        assert!(!config.recording_enabled);
        assert_eq!(config.retention_days, Config::default().retention_days);
        assert_eq!(config.recording, RecordingConfig::default());
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_rather_than_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        assert_eq!(Config::load_from(&path).unwrap(), Config::default());
        assert!(
            !path.exists(),
            "the unreadable file must not be left in place"
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("config.json.bad")).unwrap(),
            "{ this is not json",
            "the original must be recoverable by hand"
        );
    }

    #[test]
    fn summaries_are_off_until_a_provider_is_chosen_and_configured() {
        let mut inference = InferenceConfig::default();
        assert_eq!(inference.provider, InferenceProvider::Disabled);
        assert!(!inference.is_usable());

        // Selecting the cloud provider is not on its own consent to use it.
        inference.provider = InferenceProvider::Anthropic;
        assert!(!inference.is_usable());
        inference.cloud_consent = true;
        assert!(inference.is_usable());

        // The local provider needs a model on disk, consent or not.
        inference.provider = InferenceProvider::Local;
        assert!(!inference.is_usable());
        inference.local_model_path = Some(PathBuf::from("C:/models/gemma.gguf"));
        assert!(inference.is_usable());
    }

    #[test]
    fn the_mcp_server_is_off_by_default_on_the_agreed_port() {
        let mcp = McpConfig::default();
        assert!(!mcp.enabled);
        assert_eq!(mcp.port, DEFAULT_MCP_PORT);
    }

    #[test]
    fn a_phase_three_config_file_gains_the_new_sections_at_their_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"recordingEnabled": true, "startOnLaunch": false, "retentionDays": 30}"#,
        )
        .unwrap();

        let config = Config::load_from(&path).unwrap();
        assert!(!config.start_on_launch);
        assert!(
            config.start_with_windows,
            "a setting the file predates arrives at its default"
        );
        assert_eq!(config.retention_days, 30);
        assert_eq!(config.inference, InferenceConfig::default());
        assert_eq!(config.mcp, McpConfig::default());
    }

    #[test]
    fn the_dropdown_offers_seven_models_across_three_providers() {
        let ids: Vec<&str> = CLOUD_MODELS.iter().map(|choice| choice.id).collect();
        assert_eq!(
            ids,
            vec![
                "claude-haiku-4-5",
                "claude-sonnet-5",
                "claude-opus-5",
                "gpt-5.6-luna",
                "gpt-5.6-terra",
                "gpt-5.6-sol",
                "gemini-flash-latest",
            ]
        );
        assert!(
            ids.iter()
                .all(|id| !id.contains("2025") && !id.contains("2026")),
            "a dated snapshot identifier will go stale: {ids:?}"
        );
    }

    #[test]
    fn every_offered_model_names_the_provider_that_serves_it() {
        for choice in CLOUD_MODELS {
            assert!(
                choice.provider.is_cloud(),
                "{} is not served by a cloud provider",
                choice.id
            );
            assert_eq!(provider_for_model(choice.id), Some(choice.provider));
            assert!(!choice.name.is_empty() && !choice.note.is_empty());
        }
        assert_eq!(
            provider_for_model("gpt-5.6-luna"),
            Some(InferenceProvider::OpenAi)
        );
        assert_eq!(
            provider_for_model("gemini-flash-latest"),
            Some(InferenceProvider::Google)
        );
        assert_eq!(provider_for_model("something-else"), None);
    }

    #[test]
    fn the_default_model_is_one_of_the_offered_ones() {
        assert_eq!(InferenceConfig::default().cloud_model, DEFAULT_CLOUD_MODEL);
        assert!(cloud_model(DEFAULT_CLOUD_MODEL).is_some());
    }

    #[test]
    fn the_models_that_reason_are_marked_as_such() {
        assert!(!cloud_model("claude-haiku-4-5").unwrap().supports_effort);
        assert!(cloud_model("claude-sonnet-5").unwrap().supports_effort);
        assert!(cloud_model("claude-opus-5").unwrap().supports_effort);
        assert!(cloud_model("gpt-5.6-luna").unwrap().supports_effort);
        assert!(!cloud_model("gemini-flash-latest").unwrap().supports_effort);
    }

    #[test]
    fn a_provider_serializes_under_the_name_the_window_uses() {
        let json = serde_json::to_string(&InferenceProvider::OpenAi).unwrap();
        assert_eq!(json, r#""openai""#);
        assert_eq!(InferenceProvider::OpenAi.as_str(), "openai");
        assert_eq!(
            serde_json::from_str::<InferenceProvider>(r#""google""#).unwrap(),
            InferenceProvider::Google
        );
    }

    #[test]
    fn a_model_set_by_hand_is_accepted_and_left_alone() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"inference":{"provider":"anthropic","cloudModel":"claude-opus-4-8"}}"#,
        )
        .unwrap();

        let config = Config::load_from(&path).unwrap();
        assert_eq!(config.inference.cloud_model, "claude-opus-4-8");
        assert!(config.inference.choice().is_none());
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        Config::default().save_to(&path).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["config.json".to_string()]);
    }
}
