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

/// Summaries the user chose to keep, as Markdown.
///
/// Unlike everything else under this tree, the library is not derived and is never
/// rebuilt. Deleting `summaries/` costs nothing; deleting this loses what somebody
/// decided was worth keeping.
pub fn library_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("library"))
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

/// Write a file so that a reader never sees a half-written one.
///
/// The content goes to a temporary beside the destination and is renamed over it, which
/// is atomic on NTFS: a crash mid-write leaves the previous file intact rather than a
/// truncated one.
///
/// The temporary carries the process id. The name used to be fixed, which was fine
/// until two copies of the application were running at once: both wrote the same
/// temporary, the first rename moved it away, and the second failed with "could not
/// replace" over a destination that was in fact perfectly intact. With a name per
/// process the two writes no longer collide and the later one simply wins, which is
/// what an atomic write is supposed to mean.
pub fn write_atomically(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    if let Some(parent) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        ensure_dir(parent)?;
    }

    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.writing", std::process::id()));
    let temporary = path.with_file_name(name);

    std::fs::write(&temporary, contents)
        .with_context(|| format!("could not write {}", temporary.display()))?;

    // A failed rename would otherwise leave the temporary behind for good, since the
    // next attempt writes to the same per-process name and would not clean it up either.
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(anyhow::Error::new(error))
            .with_context(|| format!("could not replace {}", path.display()));
    }
    Ok(())
}

/// Create every directory the application writes into.
pub fn ensure_layout() -> Result<()> {
    for dir in [
        events_dir()?,
        episodes_dir()?,
        summaries_dir()?,
        index_dir()?,
        library_dir()?,
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

    #[test]
    fn an_atomic_write_replaces_the_file_and_leaves_nothing_beside_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested").join("thing.json");

        write_atomically(&path, "first").unwrap();
        write_atomically(&path, "second").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        let beside: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(beside, vec!["thing.json".to_string()]);
    }

    /// The failure this replaced: two processes shared one `<name>.writing`, so the
    /// second rename found its own source already moved away by the first and
    /// reported "could not replace" over a destination that was never damaged.
    #[test]
    fn a_temporary_belongs_to_one_process_and_not_to_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let one = temp.path().join("a.json");
        let two = temp.path().join("b.json");

        write_atomically(&one, "x").unwrap();
        write_atomically(&two, "y").unwrap();

        assert!(!temp.path().join("a.writing").exists());
        assert!(
            !temp
                .path()
                .join(format!("a.json.{}.writing", std::process::id()))
                .exists()
        );
    }
}
