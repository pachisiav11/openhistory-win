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
}

impl Default for RecordingConfig {
    fn default() -> Self {
        RecordingConfig {
            excluded_apps: DEFAULT_EXCLUDED.iter().map(|s| (*s).to_owned()).collect(),
            capture_urls: true,
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

/// Everything the user can change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Whether the collector runs. Turning this off stops recording without
    /// uninstalling anything or discarding existing history.
    pub recording_enabled: bool,
    /// Start collecting as soon as the application launches.
    pub start_on_launch: bool,
    /// Days of history to keep. Zero means keep everything, which is the default: a
    /// personal history that silently deletes itself is not one you can rely on.
    pub retention_days: u32,
    pub recording: RecordingConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            recording_enabled: true,
            start_on_launch: true,
            retention_days: 0,
            recording: RecordingConfig::default(),
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
            capture_urls: true,
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
