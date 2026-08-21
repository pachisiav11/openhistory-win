//! The append-only event log.
//!
//! Events are written as JSON Lines, one file per day, under
//! `%APPDATA%\openhistory-win\events\YYYY-MM-DD.jsonl`. Nothing is ever rewritten
//! in place: the only write operation is an append, which means a crash can lose at
//! most the event being written and can never corrupt an earlier one.
//!
//! Days are cut on the **local** date, not UTC. A user's history is read in terms of
//! their own day — "what did I do yesterday" — and a UTC boundary would split an
//! evening's work across two files for most of the world.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate, Utc};

use crate::event::ActivityEvent;
use crate::paths;

/// The local date an event belongs to.
///
/// Falls back to today when the timestamp cannot be parsed, which only happens for a
/// hand-edited file; losing the event entirely would be worse than filing it under
/// the wrong day.
pub fn local_date_of(event: &ActivityEvent) -> NaiveDate {
    event
        .time()
        .map(|t| t.with_timezone(&Local).date_naive())
        .unwrap_or_else(|| Local::now().date_naive())
}

/// What one day's log contains, without reading the events themselves.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DayStats {
    pub events: usize,
    /// Timestamp of the last event, exactly as recorded.
    pub last_event_at: Option<String>,
}

/// Append-only writer over the day-partitioned event log.
///
/// Holds one file open at a time and rolls over when the local date changes, so a
/// session that runs past midnight keeps writing without the caller doing anything.
pub struct EventStore {
    dir: PathBuf,
    open: Option<OpenDay>,
}

struct OpenDay {
    date: NaiveDate,
    file: BufWriter<File>,
}

impl EventStore {
    /// Open the real event log under `%APPDATA%`.
    pub fn open() -> Result<Self> {
        Self::in_dir(paths::events_dir()?)
    }

    /// Open a log rooted at an explicit directory, creating it if needed.
    pub fn in_dir(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        paths::ensure_dir(&dir)?;
        Ok(EventStore { dir, open: None })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append one event to the day it belongs to.
    ///
    /// The write is flushed before returning. Events arrive a few times a minute at
    /// most, so the syscall costs nothing measurable, and it means the log on disk is
    /// always current — which matters because the process this runs in is expected to
    /// be killed at shutdown rather than closed politely.
    pub fn append(&mut self, event: &ActivityEvent) -> Result<()> {
        let date = local_date_of(event);
        let day = self.day_for(date)?;

        let line = serde_json::to_string(event).context("could not serialize an event")?;
        day.file.write_all(line.as_bytes())?;
        day.file.write_all(b"\n")?;
        day.file.flush().with_context(|| {
            format!(
                "could not write to the event log for {}",
                date.format("%Y-%m-%d")
            )
        })?;
        Ok(())
    }

    fn day_for(&mut self, date: NaiveDate) -> Result<&mut OpenDay> {
        let rolled = self.open.as_ref().is_none_or(|day| day.date != date);
        if rolled {
            let path = self.dir.join(file_name(date));
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("could not open {}", path.display()))?;
            self.open = Some(OpenDay {
                date,
                file: BufWriter::new(file),
            });
        }
        Ok(self.open.as_mut().expect("just opened"))
    }

    /// Every event recorded on a given local date, in the order they were written.
    pub fn read_day(&self, date: NaiveDate) -> Result<Vec<ActivityEvent>> {
        read_day_in(&self.dir, date)
    }

    /// Count and last timestamp for a day, without deserializing every event.
    pub fn stats(&self, date: NaiveDate) -> Result<DayStats> {
        let path = self.dir.join(file_name(date));
        let Ok(file) = File::open(&path) else {
            return Ok(DayStats::default());
        };

        let mut stats = DayStats::default();
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_str::<ActivityEvent>(&line) else {
                continue;
            };
            stats.events += 1;
            stats.last_event_at = Some(event.timestamp);
        }
        Ok(stats)
    }

    /// Local dates that have a log file, oldest first.
    pub fn recorded_days(&self) -> Result<Vec<NaiveDate>> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Ok(Vec::new());
        };

        let mut days: Vec<NaiveDate> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let stem = name.strip_suffix(".jsonl")?;
                NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()
            })
            .collect();
        days.sort_unstable();
        Ok(days)
    }

    /// Delete logs older than `keep_days` whole days before today.
    ///
    /// Returns the dates removed. `keep_days` of 0 means keep everything, so a
    /// misconfigured retention setting cannot erase a user's history.
    pub fn prune(&self, keep_days: u32) -> Result<Vec<NaiveDate>> {
        if keep_days == 0 {
            return Ok(Vec::new());
        }
        let cutoff = Local::now().date_naive() - chrono::Duration::days(keep_days as i64);

        let mut removed = Vec::new();
        for date in self.recorded_days()? {
            if date < cutoff {
                let path = self.dir.join(file_name(date));
                std::fs::remove_file(&path)
                    .with_context(|| format!("could not remove {}", path.display()))?;
                removed.push(date);
            }
        }
        Ok(removed)
    }
}

fn file_name(date: NaiveDate) -> String {
    format!("{}.jsonl", date.format("%Y-%m-%d"))
}

/// Read one day's events out of an arbitrary event directory.
///
/// Unreadable lines are skipped rather than failing the read. A log can end in a
/// partial line if the machine lost power mid-append, and one truncated tail must not
/// cost the user the rest of the day.
pub fn read_day_in(dir: &Path, date: NaiveDate) -> Result<Vec<ActivityEvent>> {
    let path = dir.join(file_name(date));
    let Ok(file) = File::open(&path) else {
        return Ok(Vec::new());
    };

    let mut events = Vec::new();
    let mut skipped = 0usize;
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ActivityEvent>(&line) {
            Ok(event) => events.push(event),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::warn!(
            skipped,
            path = %path.display(),
            "skipped unreadable lines in an event log"
        );
    }
    Ok(events)
}

/// Read one day's events from the real event log.
pub fn read_day(date: NaiveDate) -> Result<Vec<ActivityEvent>> {
    read_day_in(&paths::events_dir()?, date)
}

/// Today, as the user's calendar sees it.
pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// The local date of an arbitrary UTC instant.
pub fn local_date(when: DateTime<Utc>) -> NaiveDate {
    when.with_timezone(&Local).date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{ApplicationDescriptor, EventKind};

    fn at(rfc3339: &str, kind: EventKind) -> ActivityEvent {
        let when = DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc);
        ActivityEvent::at(kind, when)
    }

    #[test]
    fn appended_events_read_back_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_dir(temp.path()).unwrap();

        let written = ActivityEvent::new(EventKind::ApplicationActivated).with_application(
            ApplicationDescriptor {
                name: "Visual Studio Code".into(),
                path: r"C:\Program Files\Microsoft VS Code\Code.exe".into(),
                pid: 4242,
                bundle_id: None,
            },
        );
        store.append(&written).unwrap();

        let read = store.read_day(local_date_of(&written)).unwrap();
        assert_eq!(read, vec![written]);
    }

    #[test]
    fn events_are_filed_under_their_local_day() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_dir(temp.path()).unwrap();

        // Two instants a day apart. Whatever the host time zone is, they must land in
        // two different files, and each must be readable back from its own date.
        let first = at("2026-08-21T09:30:00.000Z", EventKind::ApplicationActivated);
        let second = at("2026-08-22T09:30:00.000Z", EventKind::ApplicationActivated);
        store.append(&first).unwrap();
        store.append(&second).unwrap();

        let first_day = local_date_of(&first);
        let second_day = local_date_of(&second);
        assert_ne!(first_day, second_day);

        assert_eq!(store.read_day(first_day).unwrap(), vec![first]);
        assert_eq!(store.read_day(second_day).unwrap(), vec![second]);
        assert_eq!(store.recorded_days().unwrap(), vec![first_day, second_day]);
    }

    #[test]
    fn a_reopened_store_appends_rather_than_truncating() {
        let temp = tempfile::tempdir().unwrap();
        let event = ActivityEvent::new(EventKind::CollectorStarted);
        let date = local_date_of(&event);

        EventStore::in_dir(temp.path())
            .unwrap()
            .append(&event)
            .unwrap();
        EventStore::in_dir(temp.path())
            .unwrap()
            .append(&event)
            .unwrap();

        assert_eq!(store_at(temp.path()).read_day(date).unwrap().len(), 2);
    }

    #[test]
    fn a_truncated_tail_does_not_cost_the_rest_of_the_day() {
        let temp = tempfile::tempdir().unwrap();
        let event = ActivityEvent::new(EventKind::CollectorStarted);
        let date = local_date_of(&event);

        let mut store = EventStore::in_dir(temp.path()).unwrap();
        store.append(&event).unwrap();
        drop(store);

        // Simulate a machine that lost power part-way through an append.
        let path = temp.path().join(file_name(date));
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"version":1,"id":"half-writ"#).unwrap();
        drop(file);

        let read = read_day_in(temp.path(), date).unwrap();
        assert_eq!(read, vec![event]);
    }

    #[test]
    fn non_ascii_titles_survive_the_round_trip_as_utf8() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_dir(temp.path()).unwrap();

        // Real window titles carry emoji, accents and CJK. The log is UTF-8 and the
        // macOS build reads it directly, so these must be written as characters rather
        // than mangled into the host's ANSI code page.
        let title =
            "Geometry ONE SHOT \u{1F525} \u{2014} \u{5E7E}\u{4F55}\u{5B66} \u{2014} Caf\u{E9}";
        let written = ActivityEvent::new(EventKind::WindowChanged).with_window_title(title);
        store.append(&written).unwrap();

        let date = local_date_of(&written);
        assert_eq!(
            store.read_day(date).unwrap()[0].window_title.as_deref(),
            Some(title)
        );

        let bytes = std::fs::read(temp.path().join(file_name(date))).unwrap();
        let text = String::from_utf8(bytes).expect("the log must be valid UTF-8");
        assert!(
            text.contains(title),
            "the title must be stored as characters, not escapes"
        );
    }

    #[test]
    fn stats_report_the_count_and_the_last_timestamp() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = EventStore::in_dir(temp.path()).unwrap();

        let first = at("2026-08-21T09:30:00.000Z", EventKind::CollectorStarted);
        let last = at("2026-08-21T09:45:00.000Z", EventKind::ApplicationActivated);
        let date = local_date_of(&first);
        assert_eq!(
            date,
            local_date_of(&last),
            "fixture must stay within one local day"
        );

        store.append(&first).unwrap();
        store.append(&last).unwrap();

        let stats = store.stats(date).unwrap();
        assert_eq!(stats.events, 2);
        assert_eq!(
            stats.last_event_at.as_deref(),
            Some(last.timestamp.as_str())
        );
    }

    #[test]
    fn an_unrecorded_day_is_empty_rather_than_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::in_dir(temp.path()).unwrap();
        let date = NaiveDate::from_ymd_opt(2001, 1, 1).unwrap();

        assert!(store.read_day(date).unwrap().is_empty());
        assert_eq!(store.stats(date).unwrap(), DayStats::default());
        assert!(store.recorded_days().unwrap().is_empty());
    }

    #[test]
    fn pruning_keeps_recent_days_and_never_empties_the_log() {
        let temp = tempfile::tempdir().unwrap();
        let store = EventStore::in_dir(temp.path()).unwrap();

        let today = Local::now().date_naive();
        for offset in [0i64, 3, 40] {
            let date = today - chrono::Duration::days(offset);
            std::fs::write(temp.path().join(file_name(date)), "").unwrap();
        }

        // A retention of zero means "keep everything", not "delete everything".
        assert!(store.prune(0).unwrap().is_empty());
        assert_eq!(store.recorded_days().unwrap().len(), 3);

        let removed = store.prune(30).unwrap();
        assert_eq!(removed, vec![today - chrono::Duration::days(40)]);
        assert_eq!(store.recorded_days().unwrap().len(), 2);
    }

    fn store_at(dir: &Path) -> EventStore {
        EventStore::in_dir(dir).unwrap()
    }
}
