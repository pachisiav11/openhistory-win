//! Where the day went.
//!
//! Rollups measure with an episode's *active* time, never its elapsed time. An episode
//! that spans an hour because the machine sat untouched is an hour on the timeline and
//! a few minutes of work, and a report that cannot tell those apart is worse than no
//! report.
//!
//! An episode that crosses an hour boundary has its active time divided between the
//! hours in proportion to how much of its span fell in each, which is the closest
//! attribution available without per-minute sampling.

use std::collections::BTreeMap;

use chrono::{DateTime, Local, NaiveDate, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::episode::Episode;

/// Time spent in one application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUsage {
    pub app: String,
    pub active_ms: i64,
    pub episodes: usize,
}

/// One hour of the local day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HourlyRollup {
    /// Local hour, 0 to 23.
    pub hour: u32,
    pub active_ms: i64,
    /// Applications used in this hour, most time first.
    pub apps: Vec<AppUsage>,
    /// Episodes that overlapped this hour, in the order they started.
    pub episode_ids: Vec<String>,
}

impl HourlyRollup {
    /// The application this hour was mostly spent in.
    pub fn leading_app(&self) -> Option<&str> {
        self.apps.first().map(|usage| usage.app.as_str())
    }
}

/// A whole local day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyRollup {
    pub date: String,
    pub active_ms: i64,
    pub episodes: usize,
    /// Applications used, most time first.
    pub apps: Vec<AppUsage>,
    /// Every hour that had any activity, earliest first.
    pub hours: Vec<HourlyRollup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_activity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<String>,
    /// How many episodes were private. Their time is counted; nothing else about them
    /// is, because nothing else was recorded.
    pub private_episodes: usize,
}

/// Roll a day's episodes up into hourly and daily totals.
pub fn roll_up(date: NaiveDate, episodes: &[Episode]) -> DailyRollup {
    let mut hours: BTreeMap<u32, HourBuilder> = BTreeMap::new();
    let mut totals: BTreeMap<String, (i64, usize)> = BTreeMap::new();
    let mut active_ms = 0i64;
    let mut private_episodes = 0usize;
    let mut first: Option<DateTime<Utc>> = None;
    let mut last: Option<DateTime<Utc>> = None;

    for episode in episodes {
        let (Some(start), Some(end)) = (episode.started_at(), episode.ended_at()) else {
            continue;
        };

        active_ms += episode.active_ms;
        if episode.is_private {
            private_episodes += 1;
        }
        first = Some(first.map_or(start, |current| current.min(start)));
        last = Some(last.map_or(end, |current| current.max(end)));

        let entry = totals.entry(episode.app.clone()).or_insert((0, 0));
        entry.0 += episode.active_ms;
        entry.1 += 1;

        for (hour, share) in spread_over_hours(start, end, episode.active_ms) {
            let builder = hours.entry(hour).or_insert_with(|| HourBuilder::new(hour));
            builder.add(&episode.app, share, &episode.id);
        }
    }

    DailyRollup {
        date: date.format("%Y-%m-%d").to_string(),
        active_ms,
        episodes: episodes.len(),
        apps: rank(totals),
        hours: hours.into_values().map(HourBuilder::finish).collect(),
        first_activity: first.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        last_activity: last.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        private_episodes,
    }
}

/// Split an episode's active time across the local hours its span touches.
///
/// A zero-length episode belongs entirely to the hour it happened in; anything longer
/// is divided by how much of its span each hour holds.
fn spread_over_hours(start: DateTime<Utc>, end: DateTime<Utc>, active_ms: i64) -> Vec<(u32, i64)> {
    let start_local = start.with_timezone(&Local);
    let span_ms = (end - start).num_milliseconds().max(0);

    if span_ms == 0 {
        return vec![(start_local.hour(), active_ms)];
    }

    let mut shares: Vec<(u32, i64)> = Vec::new();
    let mut assigned = 0i64;
    let mut cursor = start_local;

    while cursor < end.with_timezone(&Local) {
        let hour = cursor.hour();
        let next_hour = next_hour_boundary(cursor);
        let slice_end = next_hour.min(end.with_timezone(&Local));
        let slice_ms = (slice_end - cursor).num_milliseconds().max(0);

        let share = active_ms * slice_ms / span_ms;
        shares.push((hour, share));
        assigned += share;
        cursor = slice_end;

        // A daylight-saving jump can leave `next_hour_boundary` at or behind the
        // cursor. Stopping is better than looping forever over a rare edge.
        if slice_ms == 0 {
            break;
        }
    }

    if shares.is_empty() {
        return vec![(start_local.hour(), active_ms)];
    }

    // Integer division loses a millisecond or two; give the remainder to the hour the
    // episode started in, so the hourly totals always add up to the daily one.
    if let Some(first) = shares.first_mut() {
        first.1 += active_ms - assigned;
    }
    shares
}

fn next_hour_boundary(when: DateTime<Local>) -> DateTime<Local> {
    let truncated = when
        .with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(when);
    truncated + chrono::Duration::hours(1)
}

fn rank(totals: BTreeMap<String, (i64, usize)>) -> Vec<AppUsage> {
    let mut ranked: Vec<AppUsage> = totals
        .into_iter()
        .map(|(app, (active_ms, episodes))| AppUsage {
            app,
            active_ms,
            episodes,
        })
        .collect();
    // Most time first; ties broken by name so the output is stable.
    ranked.sort_by(|a, b| {
        b.active_ms
            .cmp(&a.active_ms)
            .then_with(|| a.app.cmp(&b.app))
    });
    ranked
}

struct HourBuilder {
    hour: u32,
    active_ms: i64,
    apps: BTreeMap<String, (i64, usize)>,
    episode_ids: Vec<String>,
}

impl HourBuilder {
    fn new(hour: u32) -> Self {
        HourBuilder {
            hour,
            active_ms: 0,
            apps: BTreeMap::new(),
            episode_ids: Vec::new(),
        }
    }

    fn add(&mut self, app: &str, active_ms: i64, episode_id: &str) {
        self.active_ms += active_ms;
        let entry = self.apps.entry(app.to_owned()).or_insert((0, 0));
        entry.0 += active_ms;
        entry.1 += 1;
        if !self.episode_ids.iter().any(|id| id == episode_id) {
            self.episode_ids.push(episode_id.to_owned());
        }
    }

    fn finish(self) -> HourlyRollup {
        HourlyRollup {
            hour: self.hour,
            active_ms: self.active_ms,
            apps: rank(self.apps),
            episode_ids: self.episode_ids,
        }
    }
}

/// The local wall-clock hour an instant falls in.
pub fn local_hour(when: DateTime<Utc>) -> u32 {
    when.with_timezone(&Local).hour()
}

/// The instant a local hour begins, for rendering an hour's range.
pub fn hour_start(date: NaiveDate, hour: u32) -> Option<DateTime<Utc>> {
    let naive = date.and_hms_opt(hour, 0, 0)?;
    Local
        .from_local_datetime(&naive)
        .single()
        .map(|local| local.with_timezone(&Utc))
}

/// Milliseconds rendered the way a person reads them: `2h 15m`, `45m`, `30s`.
pub fn human_duration(millis: i64) -> String {
    let seconds = millis / 1000;
    let (hours, minutes) = (seconds / 3600, (seconds % 3600) / 60);
    match (hours, minutes) {
        (0, 0) => format!("{}s", seconds.max(0)),
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 21).unwrap()
    }

    /// Build an episode from local wall-clock minutes, so hour attribution can be
    /// asserted without knowing the machine's time zone.
    fn episode(id: &str, app: &str, from_hour: u32, minute: u32, minutes: i64) -> Episode {
        let start = hour_start(date(), from_hour).unwrap() + Duration::minutes(minute as i64);
        let end = start + Duration::minutes(minutes);
        Episode {
            id: id.to_owned(),
            date: "2026-08-21".into(),
            app: app.to_owned(),
            app_path: None,
            title: None,
            titles: Vec::new(),
            urls: Vec::new(),
            start: start.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            end: end.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            duration_ms: Duration::minutes(minutes).num_milliseconds(),
            active_ms: Duration::minutes(minutes).num_milliseconds(),
            event_count: 2,
            is_private: false,
        }
    }

    #[test]
    fn a_day_totals_its_episodes() {
        let episodes = [
            episode("a", "Visual Studio Code", 9, 0, 40),
            episode("b", "Slack", 9, 40, 20),
        ];

        let rollup = roll_up(date(), &episodes);
        assert_eq!(rollup.date, "2026-08-21");
        assert_eq!(rollup.episodes, 2);
        assert_eq!(rollup.active_ms, Duration::minutes(60).num_milliseconds());
        assert_eq!(rollup.apps[0].app, "Visual Studio Code");
        assert_eq!(
            rollup.apps[0].active_ms,
            Duration::minutes(40).num_milliseconds()
        );
        assert_eq!(rollup.apps[1].app, "Slack");
    }

    #[test]
    fn applications_are_ranked_by_time_not_by_episode_count() {
        let episodes = [
            episode("a", "Slack", 9, 0, 2),
            episode("b", "Slack", 9, 10, 2),
            episode("c", "Slack", 9, 20, 2),
            episode("d", "Visual Studio Code", 10, 0, 50),
        ];

        let rollup = roll_up(date(), &episodes);
        assert_eq!(rollup.apps[0].app, "Visual Studio Code");
        assert_eq!(rollup.apps[1].episodes, 3);
    }

    #[test]
    fn an_episode_that_crosses_an_hour_is_split_between_them() {
        // Thirty minutes from 09:45, so fifteen either side of ten o'clock.
        let episodes = [episode("a", "Visual Studio Code", 9, 45, 30)];

        let rollup = roll_up(date(), &episodes);
        assert_eq!(rollup.hours.len(), 2);
        assert_eq!(rollup.hours[0].hour, 9);
        assert_eq!(rollup.hours[1].hour, 10);
        assert_eq!(
            rollup.hours[0].active_ms,
            Duration::minutes(15).num_milliseconds()
        );
        assert_eq!(
            rollup.hours[1].active_ms,
            Duration::minutes(15).num_milliseconds()
        );
    }

    #[test]
    fn hourly_totals_always_add_up_to_the_daily_total() {
        // Deliberately awkward: a span that divides unevenly across three hours.
        let episodes = [
            episode("a", "Visual Studio Code", 8, 37, 143),
            episode("b", "Slack", 13, 11, 7),
        ];

        let rollup = roll_up(date(), &episodes);
        let summed: i64 = rollup.hours.iter().map(|hour| hour.active_ms).sum();
        assert_eq!(
            summed, rollup.active_ms,
            "rounding must not lose or invent time"
        );
    }

    #[test]
    fn idle_time_is_not_counted_as_work() {
        // An hour on the clock, five minutes of evidence.
        let mut idle = episode("a", "Visual Studio Code", 9, 0, 60);
        idle.active_ms = Duration::minutes(5).num_milliseconds();

        let rollup = roll_up(date(), &[idle]);
        assert_eq!(rollup.active_ms, Duration::minutes(5).num_milliseconds());
        assert_eq!(
            rollup.hours.iter().map(|h| h.active_ms).sum::<i64>(),
            Duration::minutes(5).num_milliseconds()
        );
    }

    #[test]
    fn the_leading_application_of_an_hour_is_the_one_with_the_most_time() {
        let episodes = [
            episode("a", "Slack", 14, 0, 10),
            episode("b", "Visual Studio Code", 14, 10, 45),
        ];

        let rollup = roll_up(date(), &episodes);
        let hour = rollup.hours.iter().find(|h| h.hour == 14).unwrap();
        assert_eq!(hour.leading_app(), Some("Visual Studio Code"));
        assert_eq!(hour.episode_ids, vec!["a", "b"]);
    }

    #[test]
    fn private_episodes_are_counted_without_being_described() {
        let mut private = episode("p", "Google Chrome", 11, 0, 10);
        private.is_private = true;

        let rollup = roll_up(date(), &[private]);
        assert_eq!(rollup.private_episodes, 1);
        assert_eq!(rollup.active_ms, Duration::minutes(10).num_milliseconds());
        assert_eq!(rollup.apps[0].app, "Google Chrome");
    }

    #[test]
    fn the_day_reports_when_it_started_and_ended() {
        let episodes = [
            episode("a", "Visual Studio Code", 9, 0, 30),
            episode("b", "Slack", 17, 0, 15),
        ];

        let rollup = roll_up(date(), &episodes);
        assert_eq!(rollup.first_activity, Some(episodes[0].start.clone()));
        assert_eq!(rollup.last_activity, Some(episodes[1].end.clone()));
    }

    #[test]
    fn an_empty_day_rolls_up_to_nothing_rather_than_failing() {
        let rollup = roll_up(date(), &[]);
        assert_eq!(rollup.active_ms, 0);
        assert_eq!(rollup.episodes, 0);
        assert!(rollup.apps.is_empty());
        assert!(rollup.hours.is_empty());
        assert!(rollup.first_activity.is_none());
    }

    #[test]
    fn durations_read_the_way_people_write_them() {
        assert_eq!(human_duration(0), "0s");
        assert_eq!(human_duration(30_000), "30s");
        assert_eq!(human_duration(45 * 60_000), "45m");
        assert_eq!(human_duration(2 * 60 * 60_000), "2h");
        assert_eq!(human_duration((2 * 60 + 15) * 60_000), "2h 15m");
    }
}
