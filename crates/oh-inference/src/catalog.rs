//! The models the application offers to download, and where they land on disk.
//!
//! The list itself is data, in `catalog.json`, rather than code. Repository paths and
//! file names on Hugging Face change without warning, and a wrong one should be a
//! one-line correction to a data file, not an edit to a match arm.
//!
//! Sizes in the file are approximate and are only used before a download starts, to
//! say whether a model plausibly fits. The real size comes from the server when the
//! download begins (see [`crate::download`]), because published figures for these
//! models disagree depending on quantization.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use oh_core::paths;
use serde::{Deserialize, Serialize};

/// The curated list, embedded at build time.
const CATALOG_JSON: &str = include_str!("catalog.json");

/// A model the application knows how to fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub parameters: String,
    pub quantization: String,
    /// Hugging Face repository, `owner/name`.
    pub repo: String,
    /// File within the repository.
    pub file: String,
    /// Rough download size, for the list before anything is fetched.
    pub approximate_bytes: u64,
    /// Machine memory below which this model is not worth offering.
    pub recommended_ram_bytes: u64,
    pub note: String,
}

impl CatalogModel {
    /// Where this model's file lives once it is downloaded.
    pub fn path_in(&self, models_dir: &Path) -> PathBuf {
        models_dir.join(&self.id).join(&self.file)
    }

    /// The public download URL.
    pub fn download_url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/main/{}?download=true",
            self.repo, self.file
        )
    }
}

/// A catalog entry together with what is true of it on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    #[serde(flatten)]
    pub model: CatalogModel,
    /// The file is present and non-empty.
    pub installed: bool,
    /// Its actual size on disk, when installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// This machine has at least the recommended memory.
    pub fits_memory: bool,
}

/// The curated list, in the order it is offered.
///
/// Parsing is done once per call rather than cached: the list is five entries and this
/// is called when a settings page opens, not in a loop.
pub fn catalog() -> Vec<CatalogModel> {
    serde_json::from_str(CATALOG_JSON).expect("the embedded catalog is valid JSON")
}

pub fn find(id: &str) -> Option<CatalogModel> {
    catalog().into_iter().find(|model| model.id == id)
}

/// The catalog with each entry's state on this machine filled in.
pub fn statuses() -> Result<Vec<ModelStatus>> {
    let dir = paths::models_dir()?;
    Ok(statuses_in(&dir, total_memory_bytes()))
}

/// The same, against an explicit directory and memory figure. Used by tests.
pub fn statuses_in(models_dir: &Path, total_memory: Option<u64>) -> Vec<ModelStatus> {
    catalog()
        .into_iter()
        .map(|model| {
            let path = model.path_in(models_dir);
            let size = std::fs::metadata(&path)
                .ok()
                .map(|meta| meta.len())
                .filter(|len| *len > 0);

            ModelStatus {
                // Absent memory information is not a reason to hide a model: the user
                // knows their own machine better than a failed API call does.
                fits_memory: total_memory.is_none_or(|total| total >= model.recommended_ram_bytes),
                installed: size.is_some(),
                path: size.is_some().then(|| path.clone()),
                installed_bytes: size,
                model,
            }
        })
        .collect()
}

/// Delete a downloaded model, and its directory if that leaves it empty.
pub fn remove(id: &str) -> Result<bool> {
    let Some(model) = find(id) else {
        return Ok(false);
    };
    let dir = paths::models_dir()?;
    let path = model.path_in(&dir);
    if !path.exists() {
        return Ok(false);
    }

    std::fs::remove_file(&path).with_context(|| format!("could not delete {}", path.display()))?;
    if let Some(parent) = path.parent()
        && std::fs::read_dir(parent).is_ok_and(|mut entries| entries.next().is_none())
    {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(true)
}

/// Physical memory installed in this machine, in bytes.
#[cfg(windows)]
pub fn total_memory_bytes() -> Option<u64> {
    // SAFETY: the call fills a caller-allocated struct whose size it is told.
    unsafe {
        let mut kilobytes: u64 = 0;
        windows::Win32::System::SystemInformation::GetPhysicallyInstalledSystemMemory(
            &mut kilobytes,
        )
        .ok()?;
        Some(kilobytes * 1024)
    }
}

#[cfg(not(windows))]
pub fn total_memory_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_is_the_five_models_that_were_agreed() {
        let ids: Vec<String> = catalog().into_iter().map(|model| model.id).collect();
        assert_eq!(
            ids,
            vec![
                "gemma-4-e2b-qat",
                "gemma-4-e4b-qat",
                "qwen3.5-4b",
                "phi-4-mini",
                "qwen3.5-2b",
            ]
        );
    }

    #[test]
    fn every_entry_is_complete_enough_to_download() {
        for model in catalog() {
            assert!(!model.repo.is_empty(), "{} has no repository", model.id);
            assert!(
                model.repo.contains('/'),
                "{} has a repository that is not owner/name",
                model.id
            );
            assert!(
                model.file.ends_with(".gguf"),
                "{} does not name a GGUF file",
                model.id
            );
            assert!(model.approximate_bytes > 0, "{} has no size", model.id);
            assert!(
                model.download_url().starts_with("https://huggingface.co/"),
                "{} does not resolve to Hugging Face",
                model.id
            );
        }
    }

    #[test]
    fn nothing_is_marked_installed_when_the_directory_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let statuses = statuses_in(temp.path(), Some(32_000_000_000));

        assert_eq!(statuses.len(), 5);
        assert!(statuses.iter().all(|status| !status.installed));
        assert!(statuses.iter().all(|status| status.fits_memory));
        assert!(statuses.iter().all(|status| status.path.is_none()));
    }

    #[test]
    fn a_downloaded_file_is_reported_with_its_real_size() {
        let temp = tempfile::tempdir().unwrap();
        let model = find("qwen3.5-2b").unwrap();
        let path = model.path_in(temp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not really a model").unwrap();

        let status = statuses_in(temp.path(), None)
            .into_iter()
            .find(|status| status.model.id == "qwen3.5-2b")
            .unwrap();

        assert!(status.installed);
        assert_eq!(status.installed_bytes, Some(18));
        assert_eq!(status.path, Some(path));
    }

    #[test]
    fn an_empty_file_is_a_failed_download_rather_than_an_installed_model() {
        let temp = tempfile::tempdir().unwrap();
        let path = find("phi-4-mini").unwrap().path_in(temp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();

        let status = statuses_in(temp.path(), None)
            .into_iter()
            .find(|status| status.model.id == "phi-4-mini")
            .unwrap();
        assert!(!status.installed);
    }

    #[test]
    fn a_machine_with_little_memory_is_told_which_models_do_not_fit() {
        let temp = tempfile::tempdir().unwrap();
        let statuses = statuses_in(temp.path(), Some(8_000_000_000));

        let fitting: Vec<&str> = statuses
            .iter()
            .filter(|status| status.fits_memory)
            .map(|status| status.model.id.as_str())
            .collect();
        assert_eq!(fitting, vec!["gemma-4-e2b-qat", "qwen3.5-2b"]);
    }

    #[test]
    fn an_unknown_identifier_finds_nothing() {
        assert!(find("llama-2-70b").is_none());
    }
}
