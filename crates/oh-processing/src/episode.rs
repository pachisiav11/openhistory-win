//! Turning a stream of observations into episodes of work.
//!
//! An episode is a continuous stretch spent in one application. The raw log records
//! every foreground change and every title change, which is far too granular to read;
//! grouping them is what makes a day legible.
//!
//! Episodes carry a summary of what happened, not the events themselves. The raw log
//! is the source of truth and is cheap to re-read, so duplicating it here would double
//! the storage for nothing.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, Utc};
use oh_core::{ActivityEvent, EventKind};
use serde::{Deserialize, Serialize};

/// The most time a single silence may contribute to an episode's active time.
///
/// The collector reports changes, not presence, so a stretch with no events is not
/// evidence of work — someone reading a page produces nothing at all. Time beyond this
/// is real elapsed time but is not counted as time spent.
pub const ACTIVE_GAP: Duration = Duration::minutes(5);

/// How long a silence must be before it ends the episode.
///
/// The original plan split at five minutes, which is wrong for a foreground collector:
/// staying in one file for ten minutes emits no events at all and would be torn into
/// two entries. A quarter of an hour with no foreground change and no title change is
/// more likely absence than concentration, and lock and sleep close an episode exactly
/// anyway, so this only has to catch a machine left running.
pub const IDLE_SPLIT: Duration = Duration::minutes(15);

/// A continuous stretch of work in one application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
    pub id: String,
    /// Local date this episode is filed under, `YYYY-MM-DD`.
    pub date: String,
    /// Display name of the application, as the collector reported it.
    pub app: String,
    /// Full path to the executable, when one was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_path: Option<String>,
    /// The most representative window title: the one held longest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Every distinct title seen, in the order they first appeared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub titles: Vec<String>,
    /// Every distinct URL visited, in the order they first appeared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
    pub start: String,
    pub end: String,
    /// Elapsed time from the first event to the last, in milliseconds.
    pub duration_ms: i64,
    /// The part of `duration_ms` there is evidence for. Silences count towards this
    /// only up to [`ACTIVE_GAP`]. Rollups measure with this, not with `duration_ms`.
    pub active_ms: i64,
    pub event_count: usize,
    /// True for a private browsing session. Such an episode carries no title and no
    /// URL, and consumers must not try to describe it beyond the application.
    pub is_private: bool,
}

impl Episode {
    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        parse(&self.start)
    }

    pub fn ended_at(&self) -> Option<DateTime<Utc>> {
        parse(&self.end)
    }

    /// A one-line description, for search results and summaries.
    pub fn describe(&self) -> String {
        if self.is_private {
            return format!("{} (private)", self.app);
        }
        match &self.title {
            Some(title) => format!("{} — {title}", self.app),
            None => self.app.clone(),
        }
    }
}

fn parse(stamp: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn stamp(when: DateTime<Utc>) -> String {
    when.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// How much of an interval counts as time spent.
fn countable(interval: Duration) -> i64 {
    interval
        .clamp(Duration::zero(), ACTIVE_GAP)
        .num_milliseconds()
}

/// Event kinds that describe a window the user is looking at.
///
/// Everything else — the collector starting, the session locking, a process exiting —
/// marks a boundary rather than belonging inside an episode.
fn is_window_event(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::ApplicationActivated
            | EventKind::WindowChanged
            | EventKind::UrlChanged
            | EventKind::PrivacyBoundary
    )
}

/// Event kinds that mean the user stopped, at a time we know exactly.
fn is_away_event(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::SessionLocked | EventKind::ScreenSlept | EventKind::ApplicationTerminated
    )
}

/// Group one day's events into episodes.
///
/// Events are expected in the order they were recorded. Out-of-order timestamps are
/// tolerated: they simply do not extend an episode backwards.
pub fn detect_episodes(date: NaiveDate, events: &[ActivityEvent]) -> Vec<Episode> {
    let date_label = date.format("%Y-%m-%d").to_string();
    let mut episodes = Vec::new();
    let mut open: Option<Builder> = None;

    for event in events {
        let Some(when) = event.time() else { continue };

        if is_away_event(event.kind) {
            // The user stopped at a moment we know, so close at that moment rather
            // than guessing from the last thing they touched.
            if let Some(builder) = open.take() {
                episodes.push(builder.close(when));
            }
            continue;
        }

        if !is_window_event(event.kind) {
            continue;
        }

        let app = event
            .application
            .as_ref()
            .map(|a| a.name.clone())
            .unwrap_or_default();
        let private = event.is_private();

        match open.as_mut() {
            None => open = Some(Builder::new(&date_label, event, when, app, private)),
            Some(builder) => {
                let gap = when - builder.last;
                if gap > IDLE_SPLIT {
                    // A long silence: we do not know what happened in it, so the
                    // episode ends where its evidence ends.
                    let last = builder.last;
                    episodes.push(open.take().expect("checked").close(last));
                    open = Some(Builder::new(&date_label, event, when, app, private));
                } else if builder.app != app || builder.is_private != private {
                    episodes.push(open.take().expect("checked").close(when));
                    open = Some(Builder::new(&date_label, event, when, app, private));
                } else {
                    builder.absorb(event, when);
                }
            }
        }
    }

    if let Some(builder) = open {
        let last = builder.last;
        episodes.push(builder.close(last));
    }
    episodes
}

struct Builder {
    id: String,
    date: String,
    app: String,
    app_path: Option<String>,
    is_private: bool,
    start: DateTime<Utc>,
    last: DateTime<Utc>,
    active_ms: i64,
    event_count: usize,
    titles: Vec<String>,
    urls: Vec<String>,
    seen_titles: BTreeSet<String>,
    seen_urls: BTreeSet<String>,
    /// Accumulated time each title was on screen, so the episode can name the one the
    /// user actually spent the session in rather than the one they happened to open
    /// with.
    title_time: Vec<(String, i64)>,
    current_title: Option<String>,
}

impl Builder {
    fn new(
        date: &str,
        event: &ActivityEvent,
        when: DateTime<Utc>,
        app: String,
        is_private: bool,
    ) -> Self {
        let mut builder = Builder {
            // Deterministic: reprocessing a day must produce the same identifiers, or
            // the search index would fill with duplicates of the same episode.
            id: format!("{date}#{}", when.timestamp_millis()),
            date: date.to_owned(),
            app,
            app_path: event.application.as_ref().map(|a| a.path.clone()),
            is_private,
            start: when,
            last: when,
            active_ms: 0,
            event_count: 0,
            titles: Vec::new(),
            urls: Vec::new(),
            seen_titles: BTreeSet::new(),
            seen_urls: BTreeSet::new(),
            title_time: Vec::new(),
            current_title: None,
        };
        builder.absorb(event, when);
        builder
    }

    fn absorb(&mut self, event: &ActivityEvent, when: DateTime<Utc>) {
        self.event_count += 1;

        // Credit the time since the last event to whatever was on screen for it.
        let elapsed = countable(when - self.last);
        self.active_ms += elapsed;
        if let Some(title) = self.current_title.clone() {
            self.credit(&title, elapsed);
        }
        if when > self.last {
            self.last = when;
        }

        if self.is_private {
            // A private episode records nothing but its own existence.
            return;
        }

        if let Some(title) = event.window_title.as_ref().filter(|t| !t.trim().is_empty()) {
            if self.seen_titles.insert(title.clone()) {
                self.titles.push(title.clone());
            }
            self.current_title = Some(title.clone());
        }

        if let Some(url) = event.browser.as_ref().and_then(|b| b.url.as_ref())
            && self.seen_urls.insert(url.clone())
        {
            self.urls.push(url.clone());
        }
    }

    fn credit(&mut self, title: &str, millis: i64) {
        match self.title_time.iter_mut().find(|(name, _)| name == title) {
            Some((_, total)) => *total += millis,
            None => self.title_time.push((title.to_owned(), millis)),
        }
    }

    fn close(mut self, end: DateTime<Utc>) -> Episode {
        let end = end.max(self.start);
        let trailing = countable(end - self.last);
        self.active_ms += trailing;
        if let Some(title) = self.current_title.clone() {
            self.credit(&title, trailing);
        }

        // The longest-held title wins; ties go to the one seen first, which is the
        // order `title_time` is already in.
        let title = self
            .title_time
            .iter()
            .max_by_key(|(_, total)| *total)
            .map(|(name, _)| name.clone())
            .or_else(|| self.titles.first().cloned());

        Episode {
            id: self.id,
            date: self.date,
            app: self.app,
            app_path: self.app_path,
            title,
            titles: self.titles,
            urls: self.urls,
            start: stamp(self.start),
            end: stamp(end),
            duration_ms: (end - self.start).num_milliseconds(),
            active_ms: self.active_ms,
            event_count: self.event_count,
            is_private: self.is_private,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oh_core::{ApplicationDescriptor, BrowserObservation};

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 21).unwrap()
    }

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-21T09:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::minutes(minutes)
    }

    fn app(name: &str) -> ApplicationDescriptor {
        ApplicationDescriptor {
            name: name.to_owned(),
            path: format!(r"C:\{name}.exe"),
            pid: 1,
            bundle_id: None,
        }
    }

    fn activation(minutes: i64, name: &str, title: &str) -> ActivityEvent {
        ActivityEvent::at(EventKind::ApplicationActivated, at(minutes))
            .with_application(app(name))
            .with_window_title(title)
    }

    fn retitle(minutes: i64, name: &str, title: &str) -> ActivityEvent {
        ActivityEvent::at(EventKind::WindowChanged, at(minutes))
            .with_application(app(name))
            .with_window_title(title)
    }

    #[test]
    fn a_single_application_becomes_one_episode() {
        let events = [
            activation(0, "Visual Studio Code", "lib.rs"),
            retitle(10, "Visual Studio Code", "episode.rs"),
            activation(30, "Slack", "#engineering"),
        ];

        let episodes = detect_episodes(day(), &events);
        assert_eq!(episodes.len(), 2);
        assert_eq!(episodes[0].app, "Visual Studio Code");
        assert_eq!(episodes[0].event_count, 2);
        assert_eq!(episodes[0].titles, vec!["lib.rs", "episode.rs"]);
        assert_eq!(episodes[1].app, "Slack");
    }

    #[test]
    fn an_episode_ends_when_the_next_one_begins() {
        let events = [
            activation(0, "Visual Studio Code", "lib.rs"),
            activation(4, "Slack", "#engineering"),
        ];

        let episodes = detect_episodes(day(), &events);
        assert_eq!(
            episodes[0].duration_ms,
            Duration::minutes(4).num_milliseconds()
        );
        assert_eq!(episodes[0].end, episodes[1].start);
    }

    #[test]
    fn a_long_silence_splits_an_episode_and_is_not_counted_as_work() {
        let events = [
            activation(0, "Visual Studio Code", "lib.rs"),
            retitle(3, "Visual Studio Code", "episode.rs"),
            // Forty minutes of nothing. The user was not necessarily here.
            retitle(43, "Visual Studio Code", "rollup.rs"),
        ];

        let episodes = detect_episodes(day(), &events);
        assert_eq!(episodes.len(), 2, "the gap must split the session");
        assert_eq!(
            episodes[0].duration_ms,
            Duration::minutes(3).num_milliseconds(),
            "the first episode ends where its evidence ends, not at the next event"
        );
    }

    #[test]
    fn locking_the_session_closes_the_episode_at_the_moment_it_happened() {
        let events = [
            activation(0, "Visual Studio Code", "lib.rs"),
            ActivityEvent::at(EventKind::SessionLocked, at(12)),
            activation(20, "Visual Studio Code", "lib.rs"),
        ];

        let episodes = detect_episodes(day(), &events);
        assert_eq!(episodes.len(), 2);
        assert_eq!(
            episodes[0].duration_ms,
            Duration::minutes(12).num_milliseconds()
        );
        assert_eq!(episodes[0].end, stamp(at(12)));
    }

    #[test]
    fn lifecycle_events_never_open_an_episode() {
        let events = [
            ActivityEvent::at(EventKind::CollectorStarted, at(0)),
            ActivityEvent::at(EventKind::ScreenWoke, at(1)),
            ActivityEvent::at(EventKind::SessionUnlocked, at(2)),
        ];
        assert!(detect_episodes(day(), &events).is_empty());
    }

    #[test]
    fn the_title_held_longest_names_the_episode() {
        let events = [
            activation(0, "Visual Studio Code", "glanced-at.rs"),
            retitle(1, "Visual Studio Code", "worked-in.rs"),
            retitle(4, "Visual Studio Code", "glanced-at.rs"),
            activation(5, "Slack", "#engineering"),
        ];

        let episodes = detect_episodes(day(), &events);
        assert_eq!(episodes[0].title.as_deref(), Some("worked-in.rs"));
    }

    #[test]
    fn urls_are_collected_in_the_order_they_were_visited() {
        let visit = |minutes: i64, url: &str| {
            ActivityEvent::at(EventKind::UrlChanged, at(minutes))
                .with_application(app("Google Chrome"))
                .with_browser(BrowserObservation {
                    url: Some(url.to_owned()),
                    is_private: false,
                })
        };
        let events = [
            visit(0, "https://example.com/a"),
            visit(1, "https://example.com/b"),
            visit(2, "https://example.com/a"),
        ];

        let episodes = detect_episodes(day(), &events);
        assert_eq!(
            episodes[0].urls,
            vec!["https://example.com/a", "https://example.com/b"]
        );
    }

    #[test]
    fn a_private_session_is_its_own_episode_and_describes_nothing() {
        let boundary = ActivityEvent::at(EventKind::PrivacyBoundary, at(5))
            .with_application(app("Google Chrome"))
            .with_browser(BrowserObservation {
                url: None,
                is_private: true,
            });
        let events = [
            ActivityEvent::at(EventKind::ApplicationActivated, at(0))
                .with_application(app("Google Chrome"))
                .with_window_title("Ordinary browsing - Google Chrome"),
            boundary,
            activation(9, "Google Chrome", "Back to ordinary browsing"),
        ];

        let episodes = detect_episodes(day(), &events);
        assert_eq!(
            episodes.len(),
            3,
            "a private session must not merge into the one around it"
        );
        assert!(episodes[1].is_private);
        assert!(episodes[1].title.is_none());
        assert!(episodes[1].titles.is_empty());
        assert!(episodes[1].urls.is_empty());
        assert_eq!(episodes[1].describe(), "Google Chrome (private)");

        assert!(!episodes[0].is_private);
        assert!(!episodes[2].is_private);
    }

    #[test]
    fn identifiers_are_stable_across_reprocessing() {
        let events = [activation(0, "Visual Studio Code", "lib.rs")];
        let first = detect_episodes(day(), &events);
        let again = detect_episodes(day(), &events);
        assert_eq!(first, again);
    }

    #[test]
    fn a_day_that_ends_mid_episode_is_measured_by_its_evidence() {
        let events = [
            activation(0, "Visual Studio Code", "lib.rs"),
            retitle(7, "Visual Studio Code", "episode.rs"),
        ];

        let episodes = detect_episodes(day(), &events);
        assert_eq!(episodes.len(), 1);
        assert_eq!(
            episodes[0].duration_ms,
            Duration::minutes(7).num_milliseconds()
        );
    }

    #[test]
    fn a_silence_counts_as_time_spent_only_up_to_the_active_gap() {
        let events = [
            activation(0, "Visual Studio Code", "long-read.rs"),
            // Twelve minutes reading, which produces no events at all. The episode
            // holds together, but only five of those minutes are evidenced.
            retitle(12, "Visual Studio Code", "next.rs"),
            activation(13, "Slack", "#engineering"),
        ];

        let episodes = detect_episodes(day(), &events);
        assert_eq!(episodes.len(), 2);
        assert_eq!(
            episodes[0].duration_ms,
            Duration::minutes(13).num_milliseconds()
        );
        assert_eq!(
            episodes[0].active_ms,
            Duration::minutes(6).num_milliseconds(),
            "five minutes for the long silence, one for the minute before the switch"
        );
    }

    #[test]
    fn active_time_never_exceeds_elapsed_time() {
        let events = [
            activation(0, "Visual Studio Code", "a.rs"),
            retitle(1, "Visual Studio Code", "b.rs"),
            retitle(2, "Visual Studio Code", "c.rs"),
        ];

        for episode in detect_episodes(day(), &events) {
            assert!(episode.active_ms <= episode.duration_ms);
            assert!(episode.active_ms >= 0);
        }
    }

    #[test]
    fn an_empty_day_produces_no_episodes() {
        assert!(detect_episodes(day(), &[]).is_empty());
    }
}
