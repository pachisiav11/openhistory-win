//! What the collector refuses to record.

use serde::{Deserialize, Serialize};

/// Applications excluded from recording out of the box.
///
/// Password managers are the clear case: their window titles routinely name the
/// account or site a credential belongs to, which is exactly the kind of detail this
/// application should never hold.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectorConfig {
    /// Executable file stems to ignore, compared case-insensitively.
    pub excluded_apps: Vec<String>,
    /// Record browser URLs at all. Turning this off still records which browser was
    /// in front, just not where the user went.
    pub capture_urls: bool,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        CollectorConfig {
            excluded_apps: DEFAULT_EXCLUDED.iter().map(|s| (*s).to_owned()).collect(),
            capture_urls: true,
        }
    }
}

impl CollectorConfig {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_managers_are_excluded_by_default() {
        let config = CollectorConfig::default();
        assert!(config.excludes("1Password"));
        assert!(config.excludes("bitwarden"));
        assert!(config.excludes("KeePassXC"));
        assert!(!config.excludes("Code"));
        assert!(!config.excludes("chrome"));
    }

    #[test]
    fn exclusions_are_case_insensitive_and_deduplicated() {
        let mut config = CollectorConfig {
            excluded_apps: Vec::new(),
            capture_urls: true,
        };
        config.exclude("Slack");
        config.exclude("SLACK");

        assert_eq!(config.excluded_apps, vec!["slack".to_string()]);
        assert!(config.excludes("slack"));
        assert!(config.excludes("Slack"));
    }
}
