//! Written summaries of a day, and where they are kept.
//!
//! Summaries live in `summaries/YYYY-MM-DD.json`, one file per local day. They are
//! produced by `oh-inference` and read by the interface and by the MCP server, which
//! is why the type lives here rather than next to the code that generates it: neither
//! reader should have to depend on an inference provider to open a file.
//!
//! Like everything downstream of the event log, a summary is disposable. It is a
//! record of what a model said about a day, not a record of the day.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::paths;

/// What a model said about one local hour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HourSummary {
    /// Local hour, 0 to 23.
    pub hour: u32,
    pub text: String,
    /// The measured active time this summary describes, copied from the rollup so a
    /// reader does not need the rollup to render an hour.
    pub active_ms: i64,
    pub generated_at: String,
    /// Which provider wrote it: `anthropic` or `local`.
    pub provider: String,
    pub model: String,
}

/// Everything written about one local day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaySummary {
    pub date: String,
    /// The whole-day summary, absent until enough hours have been written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_generated_at: Option<String>,
    /// Hours that have a summary, earliest first.
    #[serde(default)]
    pub hours: Vec<HourSummary>,
}

impl DaySummary {
    pub fn new(date: NaiveDate) -> Self {
        DaySummary {
            date: date.format("%Y-%m-%d").to_string(),
            daily: None,
            daily_generated_at: None,
            hours: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.daily.is_none() && self.hours.is_empty()
    }

    pub fn hour(&self, hour: u32) -> Option<&HourSummary> {
        self.hours.iter().find(|written| written.hour == hour)
    }

    /// Add or replace one hour, keeping the hours ordered.
    pub fn set_hour(&mut self, summary: HourSummary) {
        match self.hours.binary_search_by_key(&summary.hour, |h| h.hour) {
            Ok(existing) => self.hours[existing] = summary,
            Err(position) => self.hours.insert(position, summary),
        }
    }

    pub fn set_daily(&mut self, text: impl Into<String>) {
        self.daily = Some(text.into());
        self.daily_generated_at = Some(now());
    }
}

/// Reads and writes the summary files under one directory.
pub struct SummaryStore {
    dir: PathBuf,
}

impl SummaryStore {
    /// Open the real summary directory under `%APPDATA%`.
    pub fn open() -> Result<Self> {
        Self::in_dir(paths::summaries_dir()?)
    }

    pub fn in_dir(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        paths::ensure_dir(&dir)?;
        Ok(SummaryStore { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, date: NaiveDate) -> PathBuf {
        self.dir.join(format!("{}.json", date.format("%Y-%m-%d")))
    }

    /// The summary for a day, or an empty one if nothing has been written yet.
    ///
    /// A file that cannot be parsed is reported as empty rather than as an error: it
    /// can be regenerated, and refusing to show a day because its summary is corrupt
    /// would be a worse trade than showing the day without one.
    pub fn load(&self, date: NaiveDate) -> DaySummary {
        let path = self.path(date);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return DaySummary::new(date);
        };
        match serde_json::from_str(&text) {
            Ok(summary) => summary,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "unreadable summary; ignoring");
                DaySummary::new(date)
            }
        }
    }

    pub fn save(&self, summary: &DaySummary) -> Result<()> {
        let date = NaiveDate::parse_from_str(&summary.date, "%Y-%m-%d")
            .with_context(|| format!("{} is not a date", summary.date))?;
        let path = self.path(date);

        let text =
            serde_json::to_string_pretty(summary).context("could not serialize a summary")?;
        let temporary = path.with_extension("json.writing");
        std::fs::write(&temporary, text)
            .with_context(|| format!("could not write {}", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        Ok(())
    }

    /// Delete the summary for one day. Used when a day is reprocessed from scratch.
    pub fn forget(&self, date: NaiveDate) -> Result<()> {
        let path = self.path(date);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("could not delete {}", path.display()))
            }
        }
    }

    /// Delete every summary. Returns how many files went.
    pub fn clear(&self) -> Result<usize> {
        let days = self.summarized_days();
        for date in &days {
            self.forget(*date)?;
        }
        Ok(days.len())
    }

    /// Days that have a summary file, oldest first.
    pub fn summarized_days(&self) -> Vec<NaiveDate> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut days: Vec<NaiveDate> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let stem = name.strip_suffix(".json")?;
                NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()
            })
            .collect();
        days.sort_unstable();
        days
    }
}

/// The current instant, formatted the way every timestamp in this application is.
pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 22).unwrap()
    }

    fn hour(hour: u32) -> HourSummary {
        HourSummary {
            hour,
            text: format!("Worked through hour {hour}."),
            active_ms: 600_000,
            generated_at: now(),
            provider: "local".into(),
            model: "test".into(),
        }
    }

    #[test]
    fn clearing_removes_every_summary() {
        let temp = tempfile::tempdir().unwrap();
        let store = SummaryStore::in_dir(temp.path()).unwrap();

        for day in [20, 21, 22] {
            let mut summary = DaySummary::new(NaiveDate::from_ymd_opt(2026, 8, day).unwrap());
            summary.set_daily("A day of work.");
            store.save(&summary).unwrap();
        }
        assert_eq!(store.summarized_days().len(), 3);

        assert_eq!(store.clear().unwrap(), 3);
        assert!(store.summarized_days().is_empty());
        assert!(store.load(date()).is_empty());
    }

    #[test]
    fn hours_stay_ordered_however_they_arrive() {
        let mut summary = DaySummary::new(date());
        summary.set_hour(hour(14));
        summary.set_hour(hour(9));
        summary.set_hour(hour(11));

        let hours: Vec<u32> = summary.hours.iter().map(|h| h.hour).collect();
        assert_eq!(hours, vec![9, 11, 14]);
    }

    #[test]
    fn writing_an_hour_twice_replaces_it() {
        let mut summary = DaySummary::new(date());
        summary.set_hour(hour(9));

        let mut revised = hour(9);
        revised.text = "Something else entirely.".into();
        summary.set_hour(revised);

        assert_eq!(summary.hours.len(), 1);
        assert_eq!(summary.hour(9).unwrap().text, "Something else entirely.");
    }

    #[test]
    fn a_summary_survives_a_save_and_load() {
        let temp = tempfile::tempdir().unwrap();
        let store = SummaryStore::in_dir(temp.path()).unwrap();

        let mut summary = DaySummary::new(date());
        summary.set_hour(hour(9));
        summary.set_daily("A quiet morning of Rust.");
        store.save(&summary).unwrap();

        assert_eq!(store.load(date()), summary);
        assert_eq!(store.summarized_days(), vec![date()]);
    }

    #[test]
    fn a_day_with_nothing_written_loads_as_empty() {
        let temp = tempfile::tempdir().unwrap();
        let store = SummaryStore::in_dir(temp.path()).unwrap();

        let summary = store.load(date());
        assert!(summary.is_empty());
        assert_eq!(summary.date, "2026-08-22");
    }

    #[test]
    fn an_unreadable_summary_reads_as_empty_rather_than_failing() {
        let temp = tempfile::tempdir().unwrap();
        let store = SummaryStore::in_dir(temp.path()).unwrap();
        std::fs::write(store.path(date()), "{ not json").unwrap();

        assert!(store.load(date()).is_empty());
    }

    #[test]
    fn forgetting_a_day_that_was_never_written_is_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let store = SummaryStore::in_dir(temp.path()).unwrap();
        store.forget(date()).unwrap();
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let temp = tempfile::tempdir().unwrap();
        let store = SummaryStore::in_dir(temp.path()).unwrap();
        store.save(&DaySummary::new(date())).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["2026-08-22.json".to_string()]);
    }
}
