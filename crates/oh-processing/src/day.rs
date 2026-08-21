//! The processing pipeline for one day.
//!
//! Reads the event log, groups it into episodes, measures it, writes the result to
//! `episodes/YYYY-MM-DD.json`, and folds it into the search index. Everything it
//! produces is derived, so a day can be reprocessed at any time and reprocessing is
//! how the index is repaired.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use oh_core::{EventStore, paths, store};
use serde::{Deserialize, Serialize};

use crate::episode::{Episode, detect_episodes};
use crate::index::{SearchHit, SearchIndex};
use crate::rollup::{DailyRollup, roll_up};

/// Everything derived from one day, kept in a single file so that reading a day is
/// one open rather than three.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DayReport {
    pub date: String,
    pub episodes: Vec<Episode>,
    pub rollup: DailyRollup,
}

impl DayReport {
    pub fn is_empty(&self) -> bool {
        self.episodes.is_empty()
    }
}

/// Owns the derived view of the history: episode files and the search index.
///
/// The index is held in memory because search is called on every keystroke. It is
/// written back when a day is processed, not on every query.
pub struct Processor {
    events_dir: PathBuf,
    episodes_dir: PathBuf,
    index_path: PathBuf,
    index: SearchIndex,
}

impl Processor {
    /// Open the real history under `%APPDATA%`.
    pub fn open() -> Result<Self> {
        Self::in_root_dirs(
            paths::events_dir()?,
            paths::episodes_dir()?,
            paths::search_index_file()?,
        )
    }

    /// Open a history rooted at an explicit directory, laid out the same way.
    pub fn in_root(root: &Path) -> Result<Self> {
        Self::in_root_dirs(
            root.join("events"),
            root.join("episodes"),
            root.join("index").join("search-index.json"),
        )
    }

    fn in_root_dirs(
        events_dir: PathBuf,
        episodes_dir: PathBuf,
        index_path: PathBuf,
    ) -> Result<Self> {
        paths::ensure_dir(&episodes_dir)?;
        let index = SearchIndex::load_from(&index_path);
        Ok(Processor {
            events_dir,
            episodes_dir,
            index_path,
            index,
        })
    }

    pub fn index(&self) -> &SearchIndex {
        &self.index
    }

    /// Derive everything for one day and persist it.
    pub fn process_day(&mut self, date: NaiveDate) -> Result<DayReport> {
        let events = store::read_day_in(&self.events_dir, date)
            .with_context(|| format!("could not read the event log for {date}"))?;

        let episodes = detect_episodes(date, &events);
        let rollup = roll_up(date, &episodes);
        let label = date.format("%Y-%m-%d").to_string();

        let report = DayReport {
            date: label.clone(),
            episodes,
            rollup,
        };
        self.write_report(&report)?;

        self.index.index_day(&label, &report.episodes);
        self.index.save_to(&self.index_path)?;

        Ok(report)
    }

    /// The stored report for a day, or `None` if it has not been processed.
    pub fn load_day(&self, date: NaiveDate) -> Result<Option<DayReport>> {
        let path = self.report_path(date);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        match serde_json::from_str(&text) {
            Ok(report) => Ok(Some(report)),
            Err(error) => {
                // A derived file that cannot be read is not a crisis: it can be
                // rebuilt from the event log whenever it is next processed.
                tracing::warn!(%error, path = %path.display(), "unreadable day report; ignoring");
                Ok(None)
            }
        }
    }

    /// The report for a day, processing it first if it is missing or out of date.
    ///
    /// This is what callers should use. Today's report goes stale continuously as the
    /// collector appends, so freshness is decided by comparing the event log against
    /// the report rather than by a timer.
    pub fn day(&mut self, date: NaiveDate) -> Result<DayReport> {
        if self.is_stale(date) {
            return self.process_day(date);
        }
        match self.load_day(date)? {
            Some(report) => Ok(report),
            None => self.process_day(date),
        }
    }

    /// True when the stored report does not reflect the current event log.
    pub fn is_stale(&self, date: NaiveDate) -> bool {
        let report = self.report_path(date);
        let Ok(report_written) = std::fs::metadata(&report).and_then(|m| m.modified()) else {
            return true;
        };

        let events = self
            .events_dir
            .join(format!("{}.jsonl", date.format("%Y-%m-%d")));
        match std::fs::metadata(&events).and_then(|m| m.modified()) {
            Ok(events_written) => events_written > report_written,
            // No event log at all: whatever was derived is as current as it can be.
            Err(_) => false,
        }
    }

    /// Reprocess every day that has an event log.
    ///
    /// This is the repair path: it rebuilds every report and the whole index from the
    /// only thing that is not derived, which is the event log itself.
    pub fn rebuild(&mut self) -> Result<Vec<NaiveDate>> {
        let days = EventStore::in_dir(&self.events_dir)?.recorded_days()?;
        self.index = SearchIndex::new();

        for date in &days {
            let events = store::read_day_in(&self.events_dir, *date)?;
            let episodes = detect_episodes(*date, &events);
            let rollup = roll_up(*date, &episodes);
            let label = date.format("%Y-%m-%d").to_string();

            let report = DayReport {
                date: label.clone(),
                episodes,
                rollup,
            };
            self.write_report(&report)?;
            self.index.index_day(&label, &report.episodes);
        }

        self.index.save_to(&self.index_path)?;
        Ok(days)
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        self.index.search(query, limit)
    }

    /// Days that have a processed report, oldest first.
    pub fn processed_days(&self) -> Result<Vec<NaiveDate>> {
        let Ok(entries) = std::fs::read_dir(&self.episodes_dir) else {
            return Ok(Vec::new());
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
        Ok(days)
    }

    fn report_path(&self, date: NaiveDate) -> PathBuf {
        self.episodes_dir
            .join(format!("{}.json", date.format("%Y-%m-%d")))
    }

    fn write_report(&self, report: &DayReport) -> Result<()> {
        let date = NaiveDate::parse_from_str(&report.date, "%Y-%m-%d")
            .with_context(|| format!("{} is not a date", report.date))?;
        let path = self.report_path(date);

        let text = serde_json::to_string(report).context("could not serialize a day report")?;
        let temporary = path.with_extension("json.writing");
        std::fs::write(&temporary, text)
            .with_context(|| format!("could not write {}", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};
    use oh_core::{ActivityEvent, ApplicationDescriptor, EventKind};

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 21).unwrap()
    }

    /// Write a day's events into a temporary history, the same way the collector does.
    fn history(events: &[ActivityEvent]) -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_dir(temp.path().join("events")).unwrap();
        for event in events {
            store.append(event).unwrap();
        }
        temp
    }

    fn at(minutes: i64) -> DateTime<Utc> {
        // Midday local, so the fixture stays inside one local date in any time zone.
        let noon = crate::rollup::hour_start(date(), 12).unwrap();
        noon + Duration::minutes(minutes)
    }

    fn activation(minutes: i64, app: &str, title: &str) -> ActivityEvent {
        ActivityEvent::at(EventKind::ApplicationActivated, at(minutes))
            .with_application(ApplicationDescriptor {
                name: app.to_owned(),
                path: format!(r"C:\{app}.exe"),
                pid: 1,
                bundle_id: None,
            })
            .with_window_title(title)
    }

    fn workday() -> Vec<ActivityEvent> {
        vec![
            ActivityEvent::at(EventKind::CollectorStarted, at(0)),
            activation(0, "Visual Studio Code", "episode.rs - openhistory-win"),
            activation(25, "Google Chrome", "Win32 accessibility - Google Chrome"),
            activation(40, "Visual Studio Code", "rollup.rs - openhistory-win"),
            activation(70, "Slack", "#engineering - Slack"),
        ]
    }

    #[test]
    fn processing_a_day_writes_a_report_and_indexes_it() {
        let temp = history(&workday());
        let mut processor = Processor::in_root(temp.path()).unwrap();

        let report = processor.process_day(date()).unwrap();
        assert_eq!(report.date, "2026-08-21");
        assert!(
            report.episodes.len() >= 2,
            "the plan's gate requires at least two episodes"
        );
        assert!(
            report.rollup.active_ms > 0,
            "the plan's gate requires time-in-app above zero"
        );

        // The report is on disk and reads back identically.
        let path = temp.path().join("episodes").join("2026-08-21.json");
        assert!(path.is_file());
        assert_eq!(processor.load_day(date()).unwrap(), Some(report));
    }

    #[test]
    fn a_processed_day_is_searchable() {
        let temp = history(&workday());
        let mut processor = Processor::in_root(temp.path()).unwrap();
        processor.process_day(date()).unwrap();

        let hits = processor.search("accessibility", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].episode.app, "Google Chrome");

        assert_eq!(
            processor.search("code", 10).len(),
            2,
            "both VS Code episodes"
        );
    }

    #[test]
    fn the_index_survives_reopening() {
        let temp = history(&workday());
        Processor::in_root(temp.path())
            .unwrap()
            .process_day(date())
            .unwrap();

        let reopened = Processor::in_root(temp.path()).unwrap();
        assert_eq!(reopened.search("accessibility", 10).len(), 1);
    }

    #[test]
    fn reprocessing_a_day_replaces_it_rather_than_accumulating() {
        let temp = history(&workday());
        let mut processor = Processor::in_root(temp.path()).unwrap();

        let first = processor.process_day(date()).unwrap();
        let again = processor.process_day(date()).unwrap();

        assert_eq!(first, again, "processing is deterministic");
        assert_eq!(processor.index().episode_count(), first.episodes.len());
    }

    #[test]
    fn rebuilding_restores_everything_from_the_event_log_alone() {
        let temp = history(&workday());
        let mut processor = Processor::in_root(temp.path()).unwrap();
        processor.process_day(date()).unwrap();

        // Lose the derived files entirely, as a disk problem or a bad upgrade might.
        std::fs::remove_file(temp.path().join("episodes").join("2026-08-21.json")).unwrap();
        std::fs::remove_file(temp.path().join("index").join("search-index.json")).unwrap();

        let mut recovered = Processor::in_root(temp.path()).unwrap();
        assert!(recovered.search("accessibility", 10).is_empty());

        let days = recovered.rebuild().unwrap();
        assert_eq!(days, vec![date()]);
        assert_eq!(recovered.search("accessibility", 10).len(), 1);
        assert!(recovered.load_day(date()).unwrap().is_some());
    }

    #[test]
    fn a_day_with_no_events_processes_to_an_empty_report() {
        let temp = tempfile::tempdir().unwrap();
        let mut processor = Processor::in_root(temp.path()).unwrap();

        let report = processor.process_day(date()).unwrap();
        assert!(report.is_empty());
        assert_eq!(report.rollup.active_ms, 0);
        assert_eq!(processor.processed_days().unwrap(), vec![date()]);
    }

    #[test]
    fn a_report_goes_stale_when_the_event_log_grows() {
        let temp = history(&workday());
        let mut processor = Processor::in_root(temp.path()).unwrap();
        processor.process_day(date()).unwrap();
        assert!(!processor.is_stale(date()));

        // The collector appends while the application is running.
        let mut store = EventStore::in_dir(temp.path().join("events")).unwrap();
        store
            .append(&activation(90, "Firefox", "release notes"))
            .unwrap();

        assert!(
            processor.is_stale(date()),
            "a grown log must invalidate the report"
        );
        let refreshed = processor.day(date()).unwrap();
        assert!(refreshed.episodes.iter().any(|e| e.app == "Firefox"));
        assert_eq!(processor.search("release", 10).len(), 1);
    }

    #[test]
    fn an_unprocessed_day_is_processed_on_demand() {
        let temp = history(&workday());
        let mut processor = Processor::in_root(temp.path()).unwrap();

        assert!(processor.load_day(date()).unwrap().is_none());
        let report = processor.day(date()).unwrap();
        assert!(!report.is_empty());
        assert!(processor.load_day(date()).unwrap().is_some());
    }

    #[test]
    fn an_unreadable_report_is_ignored_rather_than_fatal() {
        let temp = history(&workday());
        let mut processor = Processor::in_root(temp.path()).unwrap();
        processor.process_day(date()).unwrap();

        std::fs::write(
            temp.path().join("episodes").join("2026-08-21.json"),
            "{ not json",
        )
        .unwrap();

        assert!(processor.load_day(date()).unwrap().is_none());
        assert!(
            !processor.day(date()).unwrap().is_empty(),
            "it must rebuild instead"
        );
    }

    #[test]
    fn writing_a_report_leaves_no_temporary_file_behind() {
        let temp = history(&workday());
        Processor::in_root(temp.path())
            .unwrap()
            .process_day(date())
            .unwrap();

        let leftovers: Vec<String> = std::fs::read_dir(temp.path().join("episodes"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["2026-08-21.json".to_string()]);
    }
}
