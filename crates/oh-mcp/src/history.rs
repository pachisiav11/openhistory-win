//! What the server is allowed to say about the history.
//!
//! Every answer this crate produces comes from here, and everything that leaves goes
//! through [`oh_processing::PublicEpisode`] first. That reduction is the same one the
//! inference layer uses: a private session becomes an application and a span of time,
//! a URL loses its query string, and an executable path never leaves at all.
//!
//! The processor is shared with the window rather than opened a second time. Two
//! processors over one directory would each hold their own search index and each
//! believe theirs was current.

use std::sync::Arc;

use anyhow::Result;
use chrono::{Duration, NaiveDate};
use oh_core::{DaySummary, HourSummary, SummaryStore};
use oh_processing::rollup::AppUsage;
use oh_processing::{Processor, PublicEpisode, SearchHit, public_episodes};
use parking_lot::Mutex;
use serde::Serialize;

/// How far back [`History::recent`] will look for episodes before giving up.
const RECENT_DAYS: i64 = 7;

/// Everything the server may say about one local day.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayView {
    pub date: String,
    /// Measured active time across the day.
    pub active_ms: i64,
    pub episode_count: usize,
    /// How many of those were private, and so carry times only.
    pub private_episodes: usize,
    pub apps: Vec<AppUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily_summary: Option<String>,
    pub hourly_summaries: Vec<HourSummary>,
    /// Omitted when the settings restrict the server to summaries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episodes: Option<Vec<PublicEpisode>>,
}

/// One search result, reduced.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub id: String,
    pub date: String,
    pub app: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub start: String,
    pub end: String,
    pub active_ms: i64,
    pub is_private: bool,
    pub matched_terms: usize,
}

impl From<SearchHit> for SearchResult {
    fn from(hit: SearchHit) -> Self {
        let private = hit.episode.is_private;
        SearchResult {
            id: hit.episode.id,
            date: hit.episode.date,
            app: hit.episode.app,
            // A private episode is never indexed by its title, but it can still be
            // reached by application name, and its title must not come back with it.
            title: if private { None } else { hit.episode.title },
            start: hit.episode.start,
            end: hit.episode.end,
            active_ms: hit.episode.active_ms,
            is_private: private,
            matched_terms: hit.matched_terms,
        }
    }
}

/// The processed history, shared with the window.
#[derive(Clone)]
pub struct History {
    processor: Arc<Mutex<Processor>>,
    summaries: Arc<SummaryStore>,
}

impl History {
    pub fn new(processor: Arc<Mutex<Processor>>, summaries: Arc<SummaryStore>) -> Self {
        History {
            processor,
            summaries,
        }
    }

    /// Open a processor and summary store of its own. Used by the tests.
    pub fn in_root(root: &std::path::Path) -> Result<Self> {
        Ok(History::new(
            Arc::new(Mutex::new(Processor::in_root(root)?)),
            Arc::new(SummaryStore::in_dir(root.join("summaries"))?),
        ))
    }

    pub fn summary(&self, date: NaiveDate) -> DaySummary {
        self.summaries.load(date)
    }

    /// A day, with episodes when `with_episodes` and summaries either way.
    pub fn day(&self, date: NaiveDate, with_episodes: bool) -> Result<DayView> {
        let report = self.processor.lock().day(date)?;
        let summary = self.summaries.load(date);

        Ok(DayView {
            date: report.date.clone(),
            active_ms: report.rollup.active_ms,
            episode_count: report.episodes.len(),
            private_episodes: report.rollup.private_episodes,
            apps: report.rollup.apps.clone(),
            daily_summary: summary.daily,
            hourly_summaries: summary.hours,
            episodes: with_episodes.then(|| public_episodes(&report.episodes)),
        })
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.processor
            .lock()
            .search(query, limit)
            .into_iter()
            .map(SearchResult::from)
            .collect()
    }

    /// The most recent episodes, newest first, looking back over recent days.
    pub fn recent(&self, count: usize, today: NaiveDate) -> Result<Vec<PublicEpisode>> {
        let mut found: Vec<PublicEpisode> = Vec::new();

        for back in 0..RECENT_DAYS {
            if found.len() >= count {
                break;
            }
            let Some(date) = today.checked_sub_signed(Duration::days(back)) else {
                break;
            };
            // A day with nothing recorded is not an error here: the loop is walking
            // backwards through days that may simply not exist.
            let Ok(report) = self.processor.lock().day(date) else {
                continue;
            };

            let mut episodes = public_episodes(&report.episodes);
            episodes.reverse();
            found.extend(episodes);
        }

        found.truncate(count);
        Ok(found)
    }

    /// Days that have been processed, oldest first.
    pub fn days(&self) -> Result<Vec<NaiveDate>> {
        self.processor.lock().processed_days()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use oh_core::{
        ActivityEvent, ApplicationDescriptor, BrowserObservation, EventKind, EventStore,
    };

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 22).unwrap()
    }

    /// Midday local, so the fixture stays inside one local date in any time zone.
    fn at(minutes: i64) -> DateTime<Utc> {
        oh_processing::rollup::hour_start(date(), 12).unwrap() + Duration::minutes(minutes)
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

    fn retitle(minutes: i64, app: &str, title: &str) -> ActivityEvent {
        ActivityEvent::at(EventKind::WindowChanged, at(minutes))
            .with_application(ApplicationDescriptor {
                name: app.to_owned(),
                path: format!(r"C:\{app}.exe"),
                pid: 1,
                bundle_id: None,
            })
            .with_window_title(title)
    }

    fn private_browsing(minutes: i64, title: &str) -> ActivityEvent {
        activation(minutes, "Google Chrome", title).with_browser(BrowserObservation {
            url: Some("https://example.com/secret?token=abc".into()),
            is_private: true,
        })
    }

    /// Two ordinary sessions with a private one between them. Each session carries
    /// more than one event, which is what gives it evidenced active time.
    fn workday() -> Vec<ActivityEvent> {
        vec![
            activation(0, "Visual Studio Code", "collector.rs - openhistory-win"),
            retitle(10, "Visual Studio Code", "history.rs - openhistory-win"),
            private_browsing(30, "A page nobody should see"),
            private_browsing(38, "Another page nobody should see"),
            activation(60, "Slack", "#engineering - Slack"),
            retitle(68, "Slack", "#design - Slack"),
        ]
    }

    fn history() -> (tempfile::TempDir, History) {
        let temp = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_dir(temp.path().join("events")).unwrap();
        for event in workday() {
            store.append(&event).unwrap();
        }
        let history = History::in_root(temp.path()).unwrap();
        (temp, history)
    }

    #[test]
    fn a_day_reports_what_was_measured() {
        let (_temp, history) = history();
        let view = history.day(date(), true).unwrap();

        assert_eq!(view.date, "2026-08-22");
        assert!(view.active_ms > 0);
        assert_eq!(view.episode_count, 3);
        assert_eq!(view.private_episodes, 1);
        assert!(view.daily_summary.is_none());
        assert!(view.hourly_summaries.is_empty());
    }

    #[test]
    fn nothing_private_and_no_executable_path_leaves_in_a_day() {
        let (_temp, history) = history();
        let view = history.day(date(), true).unwrap();
        let json = serde_json::to_string(&view).unwrap();

        assert!(!json.contains("nobody should see"), "{json}");
        assert!(!json.contains("token=abc"), "{json}");
        assert!(!json.contains(".exe"), "{json}");
        assert!(!json.contains("appPath"), "{json}");
        // The private session is still counted, as time in an application.
        assert!(json.contains("Google Chrome"), "{json}");
    }

    #[test]
    fn a_summary_only_view_carries_no_episodes_at_all() {
        let (_temp, history) = history();
        let view = history.day(date(), false).unwrap();

        assert!(view.episodes.is_none());
        // The measurements still come through: they say how the day went without
        // saying what was in any window.
        assert!(view.active_ms > 0);
        assert!(!view.apps.is_empty());
    }

    #[test]
    fn a_search_result_never_carries_a_private_title() {
        let (_temp, history) = history();
        history.day(date(), true).unwrap();

        let hits = history.search("chrome", 10);
        assert!(!hits.is_empty());
        assert!(hits.iter().any(|hit| hit.is_private));
        for hit in &hits {
            if hit.is_private {
                assert_eq!(hit.title, None);
            }
        }
        let json = serde_json::to_string(&hits).unwrap();
        assert!(!json.contains("nobody should see"), "{json}");
    }

    #[test]
    fn recent_returns_the_newest_first_and_no_more_than_asked() {
        let (_temp, history) = history();
        let recent = history.recent(2, date()).unwrap();

        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].app, "Slack");
        assert_eq!(recent[1].app, "Google Chrome");
        assert!(recent[1].is_private);
        assert_eq!(recent[1].title, None);
    }

    #[test]
    fn a_day_with_nothing_recorded_is_an_empty_day_rather_than_a_failure() {
        let (_temp, history) = history();
        let empty = NaiveDate::from_ymd_opt(2020, 1, 1).unwrap();

        let view = history.day(empty, true).unwrap();
        assert_eq!(view.episode_count, 0);
        assert_eq!(view.active_ms, 0);
        assert!(history.recent(5, empty).unwrap().is_empty());
    }

    #[test]
    fn a_written_summary_comes_back_with_the_day() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_dir(temp.path().join("events")).unwrap();
        for event in workday() {
            store.append(&event).unwrap();
        }

        let summaries = SummaryStore::in_dir(temp.path().join("summaries")).unwrap();
        let mut written = oh_core::DaySummary::new(date());
        written.set_daily("A morning of Rust and a private browsing session.");
        summaries.save(&written).unwrap();

        let history = History::in_root(temp.path()).unwrap();
        let view = history.day(date(), false).unwrap();
        assert_eq!(
            view.daily_summary.as_deref(),
            Some("A morning of Rust and a private browsing session.")
        );
    }
}
