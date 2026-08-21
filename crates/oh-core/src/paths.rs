//! Where OpenHistory keeps its data.
//!
//! Everything lives under `%APPDATA%\openhistory-win`. Tests and the probe binaries
//! redirect the whole tree by setting `OPENHISTORY_DATA_DIR`, which keeps them from
//! writing into the real history.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveDate;

/// Environment variable that relocates the entire data tree.
pub const DATA_DIR_ENV: &str = "OPENHISTORY_DATA_DIR";

const APP_FOLDER: &str = "openhistory-win";

/// Root of the data tree.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(overridden) = std::env::var_os(DATA_DIR_ENV) {
        return Ok(PathBuf::from(overridden));
    }
    let roaming = dirs::config_dir().context("could not resolve %APPDATA%")?;
    Ok(roaming.join(APP_FOLDER))
}

pub fn events_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("events"))
}

pub fn episodes_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("episodes"))
}

pub fn summaries_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("summaries"))
}

pub fn index_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("index"))
}

pub fn config_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("config.json"))
}

pub fn tokens_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("tokens.json"))
}

/// The inverted search index over every episode.
pub fn search_index_file() -> Result<PathBuf> {
    Ok(index_dir()?.join("search-index.json"))
}

/// Directory where downloaded GGUF models are kept.
pub fn models_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("models"))
}

/// The append-only event log for one day.
pub fn events_file(date: NaiveDate) -> Result<PathBuf> {
    Ok(events_dir()?.join(format!("{}.jsonl", date.format("%Y-%m-%d"))))
}

pub fn episodes_file(date: NaiveDate) -> Result<PathBuf> {
    Ok(episodes_dir()?.join(format!("{}.json", date.format("%Y-%m-%d"))))
}

pub fn summaries_file(date: NaiveDate) -> Result<PathBuf> {
    Ok(summaries_dir()?.join(format!("{}.json", date.format("%Y-%m-%d"))))
}

/// Create a directory and everything above it, reporting which path failed.
pub fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("could not create directory {}", path.display()))
}

/// Create every directory the application writes into.
pub fn ensure_layout() -> Result<()> {
    for dir in [
        events_dir()?,
        episodes_dir()?,
        summaries_dir()?,
        index_dir()?,
    ] {
        ensure_dir(&dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The override is process-wide, so these assertions share one test to avoid
    /// racing against other tests in the same binary.
    #[test]
    fn override_relocates_the_whole_tree() {
        let temp = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded test body, no other thread reads the environment here.
        unsafe { std::env::set_var(DATA_DIR_ENV, temp.path()) };

        assert_eq!(data_dir().unwrap(), temp.path());
        assert_eq!(events_dir().unwrap(), temp.path().join("events"));
        assert_eq!(config_file().unwrap(), temp.path().join("config.json"));
        assert_eq!(
            search_index_file().unwrap(),
            temp.path().join("index").join("search-index.json")
        );

        let date = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        assert_eq!(
            events_file(date).unwrap(),
            temp.path().join("events").join("2026-08-21.jsonl")
        );
        assert_eq!(
            episodes_file(date).unwrap(),
            temp.path().join("episodes").join("2026-08-21.json")
        );

        ensure_layout().unwrap();
        assert!(temp.path().join("events").is_dir());
        assert!(temp.path().join("summaries").is_dir());

        unsafe { std::env::remove_var(DATA_DIR_ENV) };
    }
}
