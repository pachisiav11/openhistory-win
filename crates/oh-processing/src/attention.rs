//! How attention moved, measured rather than inferred.
//!
//! The rollups answer where the time went. They cannot answer whether it was held or
//! scattered, because both look identical once the minutes are added up: half an hour
//! in Word is half an hour whether it was one sitting or a hundred and four visits of
//! four seconds each.
//!
//! That distinction is the whole of what this module measures, and measuring it
//! properly needs one idea the episode stream does not have. An episode ends whenever
//! the foreground moves, so a person writing in Word from a draft open in another
//! window produces an episode every few seconds, none of which is the work. The work is
//! the whole oscillation. [`Thread`] is that: a continuous piece of work carried out
//! across one or two windows, with the switching between them counted rather than
//! mistaken for interruption.
//!
//! This is why a duration floor cannot be applied to episodes. On a real evening here,
//! forty-five per cent of the active time sat in visits between ten seconds and a
//! minute — the entire essay, written by alternating between the draft and its source.
//! Filtered out for being brief, it vanished from the log the model was given; the
//! model then reported the evening as an hour of Claude and a video, because those were
//! the only entries that survived. The floor now applies to a thread, where a hundred
//! short visits to one document add up to the half hour they actually were.
//!
//! Nothing here decides whether a stretch was distracted. Rapid switching between a
//! draft and the source it is being written from is not distraction; rapid switching
//! between a draft and a video is. The difference lives in which windows were trading
//! the foreground and whether the work resumed, which is why a thread reports its
//! partner and its interruptions separately from the raw switch count. The judgement is
//! left to the reader of the numbers.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Local, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::episode::Episode;

/// The least time a single visit must hold to be a stay rather than a passage.
///
/// Below this the window was crossed, not used: alt-tab landing somewhere on the way to
/// somewhere else. Used only to band the visits for reporting, never to discard them.
pub const GLANCE_MS: i64 = 10_000;

/// The least time a visit must hold to be a settled one.
///
/// The old naming floor, kept because the band it marks is still worth reporting — but
/// no longer a filter, for the reason in the module note.
pub const SETTLED_MS: i64 = 60_000;

/// The least active time a thread must hold before it is worth naming.
///
/// Applied to the whole piece of work rather than to any one visit inside it. Ten
/// seconds in Windows Terminal is a thread of ten seconds and stays unnamed; a hundred
/// four-second returns to one document is a thread of half an hour and is named.
pub const MIN_THREAD_MS: i64 = 60_000;

/// The longest a foreground visit by an outsider can be and still count as an
/// interruption of a thread rather than the end of it.
///
/// Two minutes is the point where whatever took the foreground stopped being a glance
/// aside and became the thing being done. A thread that survives it would report a
/// person as still writing when they had moved on.
const MAX_INTERRUPTION_MS: i64 = 120_000;

/// How far ahead to look when deciding whether a new application belongs to the work in
/// progress or is a visitor to it.
///
/// An application that takes the foreground and takes it again shortly after is part of
/// the oscillation; one that appears once between two returns to the document is an
/// interruption of it. Six is wide enough to see across a couple of crossings and
/// narrow enough not to reach into the next piece of work.
const LOOKAHEAD: usize = 6;

/// The longest silence a thread can span before it is a new one.
///
/// Episodes are contiguous by construction, so this only catches the gaps left where
/// the session locked or the collector stopped. Coming back after a quarter of an hour
/// away is resuming work, not continuing it.
const THREAD_BREAK: Duration = Duration::minutes(15);

/// How often two windows must hand the foreground straight to each other before they
/// count as halves of one piece of work rather than neighbours in time.
///
/// Four crossings is two round trips. Below that the pair is a coincidence of ordering:
/// a window opened, something checked, the window closed. Above it, on the evening this
/// was written against, the real pair crossed a hundred and eighty-eight times and the
/// next pair down crossed nine.
const MIN_COUPLING: usize = 4;

/// The most alternating pairs worth reporting. Past the first few the counts are down
/// in the ones and twos, which is noise about a stretch rather than a shape in it.
const MAX_PAIRS: usize = 6;

/// The most applications worth reporting a visit pattern for.
const MAX_APPS: usize = 10;

/// One application's share of a window of time, and how it was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppAttention {
    pub app: String,
    /// How many separate times the application took the foreground.
    pub visits: usize,
    pub active_ms: i64,
    /// The longest single visit. The distance between this and `active_ms` is the
    /// difference between a sitting and an accumulation.
    pub longest_visit_ms: i64,
    /// How many of the visits ran a minute or longer.
    pub settled_visits: usize,
}

/// Two applications that kept handing the foreground to one another.
///
/// Counted in both directions and reported once, because a person moving between a
/// document and the thing they are copying from crosses back and forth and neither
/// direction is the interesting one. The pair is what matters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alternation {
    pub a: String,
    pub b: String,
    /// Immediate handovers between the two, either way round.
    pub crossings: usize,
}

/// Something that took the foreground briefly during a thread and gave it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interruption {
    pub app: String,
    pub visits: usize,
    pub active_ms: i64,
    /// The most representative title seen, when one was recorded. What took the
    /// foreground away matters more than that something did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// A continuous piece of work, across one window or a pair of them.
///
/// The unit a summary should name. Its `active_ms` is the work's real size, which no
/// episode inside it carries, and its `crossings` is the difference between a sitting
/// and a shuttle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    /// The one or two applications the work was carried out in, most time first.
    pub apps: Vec<String>,
    pub start: String,
    pub end: String,
    /// Active time in the thread's own applications. Interrupting time is not in here.
    pub active_ms: i64,
    /// Elapsed time from the first visit to the last, interruptions included. The gap
    /// between this and `active_ms` is what the stretch cost beyond what it produced.
    pub span_ms: i64,
    /// Foreground visits to the thread's own applications.
    pub visits: usize,
    /// Handovers between the thread's two applications. Zero for a single-window
    /// thread, and a large number is a shuttle rather than a distraction.
    pub crossings: usize,
    /// Ids of the episodes the thread was built from, in order.
    pub episode_ids: Vec<String>,
    /// What took the foreground away and gave it back, most time first.
    pub interruptions: Vec<Interruption>,
}

impl Thread {
    /// The application the thread mostly ran in.
    pub fn lead(&self) -> Option<&str> {
        self.apps.first().map(String::as_str)
    }

    /// Whether the work was carried across a pair of windows rather than one.
    pub fn is_shuttle(&self) -> bool {
        self.apps.len() > 1 && self.crossings >= 4
    }

    /// Time lost to whatever interrupted the thread.
    pub fn interrupted_ms(&self) -> i64 {
        self.interruptions.iter().map(|out| out.active_ms).sum()
    }

    /// How many times the thread was broken into and resumed.
    pub fn interruption_count(&self) -> usize {
        self.interruptions.iter().map(|out| out.visits).sum()
    }

    /// Mean time between interruptions, or `None` when nothing broke in.
    ///
    /// The number that says whether a stretch could be thought in. Half an hour broken
    /// into twice is work; half an hour broken into twenty times is not, however the
    /// minutes add up.
    pub fn mean_uninterrupted_ms(&self) -> Option<i64> {
        let breaks = self.interruption_count();
        if breaks == 0 {
            return None;
        }
        Some(self.active_ms / (breaks as i64 + 1))
    }
}

/// What a window of time looked like as attention rather than as hours.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attention {
    pub active_ms: i64,
    /// Foreground visits in the window, counting a return to an application already
    /// used as a new visit.
    pub visits: usize,
    /// Handovers from one application to a different one.
    pub switches: usize,
    pub distinct_apps: usize,
    /// Active time in visits under ten seconds: windows crossed, not used.
    pub passing_ms: i64,
    /// Active time in visits between ten seconds and a minute. Where shuttled work
    /// lives, and where a duration floor does its damage.
    pub brief_ms: i64,
    /// Active time in visits of a minute or longer.
    pub settled_ms: i64,
    pub passing_visits: usize,
    pub brief_visits: usize,
    pub settled_visits: usize,
    /// Applications by time held, longest first.
    pub apps: Vec<AppAttention>,
    /// The pairs that traded the foreground most, most first.
    pub pairs: Vec<Alternation>,
    /// The window's work, longest first. Only threads worth naming are here.
    pub threads: Vec<Thread>,
}

impl Attention {
    /// Switches per hour of active time.
    ///
    /// Measured against active time rather than elapsed time on purpose. An hour spent
    /// half at the machine and half away is half an hour of attention, and rating its
    /// switching against a full hour would report it as calmer than it was.
    pub fn switches_per_hour(&self) -> f64 {
        if self.active_ms <= 0 {
            return 0.0;
        }
        self.switches as f64 * 3_600_000.0 / self.active_ms as f64
    }

    /// The mean length of a foreground visit.
    pub fn mean_visit_ms(&self) -> i64 {
        if self.visits == 0 {
            return 0;
        }
        self.active_ms / self.visits as i64
    }

    /// The share of active time that went into threads long enough to be named, 0 to 1.
    ///
    /// The honest replacement for counting long visits. Work shuttled across two
    /// windows in ten-second bursts is in here, because the thread it belongs to is
    /// long even though none of its visits were.
    pub fn threaded_share(&self) -> f64 {
        if self.active_ms <= 0 {
            return 0.0;
        }
        let threaded: i64 = self.threads.iter().map(|thread| thread.active_ms).sum();
        threaded as f64 / self.active_ms as f64
    }

    /// Whether there is enough here to say anything about attention at all.
    ///
    /// Four visits and a few minutes is not a shape, and numbers drawn from it invite a
    /// reader to find one. The prompts leave the whole section out below this.
    pub fn is_meaningful(&self) -> bool {
        self.visits >= 6 && self.active_ms >= 5 * 60_000
    }

    /// The threads one application took part in, longest first.
    pub fn threads_involving<'a>(&'a self, app: &str) -> Vec<&'a Thread> {
        self.threads
            .iter()
            .filter(|thread| thread.apps.iter().any(|name| name == app))
            .collect()
    }
}

/// Measure a window of episodes, which must already be in the order they happened.
///
/// The caller decides what the window is: one hour, one evening, a whole day. The
/// measurements mean the same thing at every scale, which is why the hour prompt and
/// the chat prompt can share them.
pub fn measure(episodes: &[&Episode]) -> Attention {
    let mut attention = Attention {
        visits: episodes.len(),
        ..Attention::default()
    };
    if episodes.is_empty() {
        return attention;
    }

    let mut apps: BTreeMap<String, AppAttention> = BTreeMap::new();
    let mut pairs: BTreeMap<(String, String), usize> = BTreeMap::new();

    for (position, episode) in episodes.iter().enumerate() {
        attention.active_ms += episode.active_ms;
        if episode.active_ms < GLANCE_MS {
            attention.passing_ms += episode.active_ms;
            attention.passing_visits += 1;
        } else if episode.active_ms < SETTLED_MS {
            attention.brief_ms += episode.active_ms;
            attention.brief_visits += 1;
        } else {
            attention.settled_ms += episode.active_ms;
            attention.settled_visits += 1;
        }

        let usage = apps
            .entry(episode.app.clone())
            .or_insert_with(|| AppAttention {
                app: episode.app.clone(),
                visits: 0,
                active_ms: 0,
                longest_visit_ms: 0,
                settled_visits: 0,
            });
        usage.visits += 1;
        usage.active_ms += episode.active_ms;
        usage.longest_visit_ms = usage.longest_visit_ms.max(episode.active_ms);
        if episode.active_ms >= SETTLED_MS {
            usage.settled_visits += 1;
        }

        if let Some(previous) = position.checked_sub(1).map(|before| episodes[before]) {
            if previous.app != episode.app {
                attention.switches += 1;
                // Ordered so the two directions land on one key: the pair is what is
                // being counted, not who went first.
                let key = if previous.app < episode.app {
                    (previous.app.clone(), episode.app.clone())
                } else {
                    (episode.app.clone(), previous.app.clone())
                };
                *pairs.entry(key).or_insert(0) += 1;
            }
        }
    }

    attention.distinct_apps = apps.len();

    let mut ranked: Vec<AppAttention> = apps.into_values().collect();
    ranked.sort_by(|left, right| {
        right
            .active_ms
            .cmp(&left.active_ms)
            .then_with(|| left.app.cmp(&right.app))
    });
    ranked.truncate(MAX_APPS);
    attention.apps = ranked;

    let mut crossings: Vec<Alternation> = pairs
        .into_iter()
        .map(|((a, b), crossings)| Alternation { a, b, crossings })
        .collect();
    crossings.sort_by(|left, right| {
        right
            .crossings
            .cmp(&left.crossings)
            .then_with(|| (&left.a, &left.b).cmp(&(&right.a, &right.b)))
    });
    // A pair that traded once is two applications that happened to follow each other.
    crossings.retain(|pair| pair.crossings >= 2);
    crossings.truncate(MAX_PAIRS);
    attention.pairs = crossings;

    attention.threads = threads(episodes);

    attention
}

/// Measure a window given by ownership rather than by reference.
pub fn measure_all(episodes: &[Episode]) -> Attention {
    let borrowed: Vec<&Episode> = episodes.iter().collect();
    measure(&borrowed)
}

/// Every episode that began within a range of local hours, in order.
///
/// The evening is a thing people actually ask about, and answering it from the whole
/// day makes a model do arithmetic on timestamps instead of reading. Filed by the hour
/// an episode began, which is how a person remembers it.
pub fn between_hours(episodes: &[Episode], from_hour: u32, to_hour: u32) -> Vec<&Episode> {
    episodes
        .iter()
        .filter(|episode| {
            local_hour(&episode.start).is_some_and(|hour| hour >= from_hour && hour <= to_hour)
        })
        .collect()
}

/// Group a window's episodes into the pieces of work they belong to.
///
/// Two passes, and the order of them is the whole trick.
///
/// The first pass finds which windows were coupled, by counting how often each pair
/// handed the foreground straight to the other across the entire window and matching the
/// strongest pairs first. This has to come first. Deciding it as the stream is walked —
/// letting whatever window turned up second become the partner — locks a thread onto the
/// first coincidence it meets, and every later visit, including all of the real work,
/// arrives as an interruption of a thread that never ends. Measured on a real evening
/// that produced one stretch holding two hundred and seventy interruptions, and the
/// evening's actual work, an essay shuttled between Word and a rendered draft, appeared
/// nowhere at all.
///
/// The second pass walks the stream with each episode labelled by its pair, and cuts a
/// new thread wherever the label changes for good. A window that takes the foreground
/// and gives it back within [`LOOKAHEAD`] visits is an interruption; one that keeps it,
/// or holds it past [`MAX_INTERRUPTION_MS`] in total, ends the thread and starts the
/// next.
pub fn threads(episodes: &[&Episode]) -> Vec<Thread> {
    let partner = pair_up(episodes);
    let labels: Vec<String> = episodes
        .iter()
        .map(|episode| label_for(&episode.app, &partner))
        .collect();

    let mut built: Vec<Thread> = Vec::new();
    let mut current: Option<Builder> = None;
    // Time spent away from the thread since it was last worked in. Consecutive, so a
    // stretch broken into ten times by six seconds each survives and one broken into
    // once for three minutes does not.
    let mut away_ms: i64 = 0;

    for (position, episode) in episodes.iter().enumerate() {
        let label = &labels[position];

        let stale = current.as_ref().is_some_and(|builder| {
            match (parse(&builder.end), parse(&episode.start)) {
                (Some(last), Some(next)) => next - last > THREAD_BREAK,
                _ => false,
            }
        });
        if stale && let Some(builder) = current.take() {
            built.push(builder.finish());
        }

        let Some(builder) = current.as_mut() else {
            current = Some(Builder::new(episode, label.clone()));
            away_ms = 0;
            continue;
        };

        if &builder.label == label {
            builder.take(episode);
            away_ms = 0;
            continue;
        }

        // An outsider. It interrupted the work if the work resumes shortly and the time
        // away has not itself become the thing being done.
        if away_ms + episode.active_ms <= MAX_INTERRUPTION_MS
            && resumes(&labels, position, &builder.label)
        {
            builder.interrupt(episode);
            away_ms += episode.active_ms;
            continue;
        }

        built.push(current.take().expect("a builder was in hand").finish());
        current = Some(Builder::new(episode, label.clone()));
        away_ms = 0;
    }

    if let Some(builder) = current {
        built.push(builder.finish());
    }

    built.retain(|thread| thread.active_ms >= MIN_THREAD_MS);
    built.sort_by(|left, right| {
        right
            .active_ms
            .cmp(&left.active_ms)
            .then_with(|| left.start.cmp(&right.start))
    });
    built
}

/// Which windows were halves of one piece of work, across the whole window of time.
///
/// Pairs are ranked by how often they handed the foreground straight to each other, and
/// taken strongest first, so an application that shuttled with two others is matched to
/// the one it really belonged with. Each application takes at most one partner: a third
/// window is an interruption or a new piece of work, and a thread that admitted every
/// application would be the whole day and would say nothing.
fn pair_up(episodes: &[&Episode]) -> BTreeMap<String, String> {
    let mut crossings: BTreeMap<(String, String), usize> = BTreeMap::new();
    for window in episodes.windows(2) {
        let (before, after) = (window[0], window[1]);
        if before.app == after.app {
            continue;
        }
        let key = if before.app < after.app {
            (before.app.clone(), after.app.clone())
        } else {
            (after.app.clone(), before.app.clone())
        };
        *crossings.entry(key).or_insert(0) += 1;
    }

    let mut ranked: Vec<((String, String), usize)> = crossings.into_iter().collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

    let mut partner: BTreeMap<String, String> = BTreeMap::new();
    for ((a, b), count) in ranked {
        if count < MIN_COUPLING {
            break;
        }
        if partner.contains_key(&a) || partner.contains_key(&b) {
            continue;
        }
        partner.insert(a.clone(), b.clone());
        partner.insert(b, a);
    }
    partner
}

/// The name of the piece of work an application belongs to.
///
/// Its own name where it stands alone, and the pair's where it is half of one. Both
/// halves must produce the same label, so the two names are ordered before joining.
fn label_for(app: &str, partner: &BTreeMap<String, String>) -> String {
    match partner.get(app) {
        Some(other) if other.as_str() < app => format!("{other}\u{1}{app}"),
        Some(other) => format!("{app}\u{1}{other}"),
        None => app.to_owned(),
    }
}

/// Whether the work resumes within a few visits of being taken away from.
fn resumes(labels: &[String], position: usize, label: &str) -> bool {
    labels
        .iter()
        .skip(position + 1)
        .take(LOOKAHEAD)
        .any(|later| later == label)
}

/// A thread under construction.
struct Builder {
    /// The piece of work this is: one application's name, or a coupled pair's.
    label: String,
    /// Time held per application, so the finished thread can name the lead one.
    apps: BTreeMap<String, i64>,
    start: String,
    end: String,
    active_ms: i64,
    visits: usize,
    crossings: usize,
    last_app: String,
    episode_ids: Vec<String>,
    interruptions: BTreeMap<String, Interruption>,
}

impl Builder {
    fn new(episode: &Episode, label: String) -> Self {
        let mut apps = BTreeMap::new();
        apps.insert(episode.app.clone(), episode.active_ms);
        Builder {
            label,
            apps,
            start: episode.start.clone(),
            end: episode.end.clone(),
            active_ms: episode.active_ms,
            visits: 1,
            crossings: 0,
            last_app: episode.app.clone(),
            episode_ids: vec![episode.id.clone()],
            interruptions: BTreeMap::new(),
        }
    }

    fn take(&mut self, episode: &Episode) {
        if self.last_app != episode.app {
            self.crossings += 1;
        }
        *self.apps.entry(episode.app.clone()).or_insert(0) += episode.active_ms;
        self.active_ms += episode.active_ms;
        self.visits += 1;
        self.last_app = episode.app.clone();
        self.end = episode.end.clone();
        self.episode_ids.push(episode.id.clone());
    }

    fn interrupt(&mut self, episode: &Episode) {
        let entry = self
            .interruptions
            .entry(episode.app.clone())
            .or_insert_with(|| Interruption {
                app: episode.app.clone(),
                visits: 0,
                active_ms: 0,
                title: None,
            });
        entry.visits += 1;
        entry.active_ms += episode.active_ms;
        // A private session records no title, and none may be supplied for it.
        if entry.title.is_none() && !episode.is_private {
            entry.title.clone_from(&episode.title);
        }
        // The interruption's time is not the thread's work, but it happened inside the
        // thread's span, so the span has to reach past it.
        self.end = episode.end.clone();
    }

    fn finish(self) -> Thread {
        let mut apps: Vec<(String, i64)> = self.apps.into_iter().collect();
        apps.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

        let mut interruptions: Vec<Interruption> = self.interruptions.into_values().collect();
        interruptions.sort_by(|left, right| {
            right
                .active_ms
                .cmp(&left.active_ms)
                .then_with(|| left.app.cmp(&right.app))
        });

        let span_ms = match (parse(&self.start), parse(&self.end)) {
            (Some(start), Some(end)) => (end - start).num_milliseconds().max(self.active_ms),
            _ => self.active_ms,
        };

        Thread {
            apps: apps.into_iter().map(|(app, _)| app).collect(),
            start: self.start,
            end: self.end,
            active_ms: self.active_ms,
            span_ms,
            visits: self.visits,
            crossings: self.crossings,
            episode_ids: self.episode_ids,
            interruptions,
        }
    }
}

fn parse(stamp: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|when| when.with_timezone(&Utc))
}

fn local_hour(stamp: &str) -> Option<u32> {
    parse(stamp).map(|when| when.with_timezone(&Local).hour())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    /// The evening these tests are set in. Nothing depends on the date; it only has to
    /// be one date.
    fn opening() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 27, 19, 0, 0).unwrap()
    }

    fn stamp(when: DateTime<Utc>) -> String {
        when.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    /// One visit. Its times are placeholders: [`sequence`] sets the real ones.
    fn visit(nth: usize, app: &str, seconds: i64) -> Episode {
        Episode {
            id: format!("2026-08-27#{nth}"),
            date: "2026-08-27".into(),
            app: app.into(),
            app_path: None,
            title: Some(format!("{app} window")),
            titles: Vec::new(),
            urls: Vec::new(),
            documents: Vec::new(),
            visible_text: Vec::new(),
            start: stamp(opening()),
            end: stamp(opening()),
            duration_ms: seconds * 1_000,
            active_ms: seconds * 1_000,
            event_count: 2,
            is_private: false,
        }
    }

    /// Lay visits out back to back, so the order they are written in is the order they
    /// happened.
    ///
    /// Fixtures have to be built this way. Stamping each visit from its own index and
    /// then inserting one in the middle leaves an hour-wide hole in the timeline, which
    /// [`THREAD_BREAK`] correctly reads as an absence and splits on — and the test then
    /// fails for a reason that has nothing to do with what it is testing.
    fn sequence(mut episodes: Vec<Episode>) -> Vec<Episode> {
        let mut at = opening();
        for episode in &mut episodes {
            episode.start = stamp(at);
            at += Duration::milliseconds(episode.active_ms);
            episode.end = stamp(at);
            episode.duration_ms = episode.active_ms;
        }
        episodes
    }

    /// Alternating visits to two windows, none of them long.
    fn shuttle(count: usize, one: &str, two: &str, seconds: i64) -> Vec<Episode> {
        (0..count)
            .map(|nth| visit(nth, if nth % 2 == 0 { one } else { two }, seconds))
            .collect()
    }

    /// The failure this module exists to prevent.
    ///
    /// Half an hour of writing, carried between a document and its source, arriving as
    /// two hundred visits of eight seconds. No visit is a minute; the work is.
    #[test]
    fn work_shuttled_between_two_windows_is_one_stretch_of_its_real_size() {
        let episodes = sequence(shuttle(200, "Microsoft Word", "Markdown Renderer", 8));
        let measured = measure_all(&episodes);

        assert_eq!(measured.threads.len(), 1, "{:?}", measured.threads);
        let work = &measured.threads[0];
        assert_eq!(work.active_ms, 200 * 8_000);
        assert_eq!(work.visits, 200);
        assert_eq!(work.crossings, 199);
        assert!(work.is_shuttle());
        assert_eq!(work.apps.len(), 2);
        // Not one second of it was in a visit long enough for the old floor to keep.
        assert_eq!(measured.settled_ms, 0);
        assert!((measured.threaded_share() - 1.0).abs() < f64::EPSILON);
    }

    /// The bug the old floor was written to fix, which must stay fixed.
    #[test]
    fn a_few_seconds_in_one_window_is_not_a_stretch_of_work() {
        let episodes = sequence(vec![visit(0, "Windows Terminal", 10)]);
        assert!(measure_all(&episodes).threads.is_empty());
    }

    /// A window that takes the foreground and gives it back is an interruption: counted
    /// as one, named, and neither lost nor allowed to end the work.
    #[test]
    fn a_window_that_breaks_in_and_gives_the_foreground_back_is_an_interruption() {
        let mut episodes = shuttle(40, "Microsoft Word", "Markdown Renderer", 20);
        episodes.insert(20, visit(100, "Slack", 15));
        let measured = measure_all(&sequence(episodes));

        assert_eq!(measured.threads.len(), 1, "{:?}", measured.threads);
        let work = &measured.threads[0];
        assert_eq!(work.interruption_count(), 1);
        assert_eq!(work.interruptions[0].app, "Slack");
        assert_eq!(work.interruptions[0].active_ms, 15_000);
        // The interrupting time is not counted as the work's own.
        assert_eq!(work.active_ms, 40 * 20_000);
        assert_eq!(work.interrupted_ms(), 15_000);
    }

    /// A window that takes the foreground and keeps it has ended the work rather than
    /// interrupted it, however alike the two look at the moment of the switch.
    #[test]
    fn a_window_that_keeps_the_foreground_ends_the_stretch() {
        let mut episodes = shuttle(20, "Microsoft Word", "Markdown Renderer", 20);
        episodes.push(visit(100, "Google Chrome", 600));
        let measured = measure_all(&sequence(episodes));

        let work = measured
            .threads
            .iter()
            .find(|thread| thread.apps.iter().any(|app| app == "Microsoft Word"))
            .expect("the writing is a stretch");
        assert_eq!(work.interruption_count(), 0);
        assert!(
            measured
                .threads
                .iter()
                .any(|thread| thread.apps == ["Google Chrome"]),
            "{:?}",
            measured.threads
        );
    }

    /// Two windows that merely follow one another once are not a pair. Pairing on a
    /// single crossing would make every neighbour in time look like coupled work.
    #[test]
    fn two_windows_that_cross_once_are_not_treated_as_one_piece_of_work() {
        let episodes = sequence(vec![
            visit(0, "Microsoft Word", 300),
            visit(1, "Google Chrome", 300),
        ]);
        let measured = measure_all(&episodes);

        assert_eq!(measured.threads.len(), 2, "{:?}", measured.threads);
        assert!(measured.threads.iter().all(|thread| thread.apps.len() == 1));
    }

    /// The pairing takes the strongest partner rather than the first one met.
    ///
    /// This is the whole reason the pairing is a pass of its own. Decided as the stream
    /// is walked, the work locks onto whatever turned up second — here Notepad — and
    /// the real shuttle never forms.
    #[test]
    fn an_application_is_paired_with_whichever_it_crossed_with_most() {
        let mut episodes = vec![
            visit(200, "Notepad", 20),
            visit(201, "Microsoft Word", 20),
            visit(202, "Notepad", 20),
        ];
        for nth in 0..30 {
            episodes.push(visit(nth, "Microsoft Word", 20));
            episodes.push(visit(nth + 100, "Markdown Renderer", 20));
        }
        let measured = measure_all(&sequence(episodes));

        let work = measured
            .threads
            .iter()
            .find(|thread| thread.crossings > 10)
            .expect("the shuttle is found");
        assert!(work.apps.iter().any(|app| app == "Markdown Renderer"));
        assert!(!work.apps.iter().any(|app| app == "Notepad"));
    }

    /// A long silence between two visits is a return to work, not a continuation of it.
    #[test]
    fn a_stretch_does_not_span_a_long_absence() {
        let mut episodes = sequence(shuttle(10, "Microsoft Word", "Markdown Renderer", 20));
        let resumed = opening() + Duration::hours(3);
        let mut later = visit(500, "Microsoft Word", 300);
        later.start = stamp(resumed);
        later.end = stamp(resumed + Duration::seconds(300));
        episodes.push(later);

        let measured = measure_all(&episodes);
        assert_eq!(measured.threads.len(), 2, "{:?}", measured.threads);
    }

    /// A private session keeps its time and gives up nothing else, wherever it lands.
    #[test]
    fn a_private_session_that_interrupts_carries_no_title() {
        let mut episodes = shuttle(40, "Microsoft Word", "Markdown Renderer", 20);
        let mut private = visit(100, "Google Chrome", 15);
        private.is_private = true;
        private.title = Some("should never be read".into());
        episodes.insert(20, private);

        let measured = measure_all(&sequence(episodes));
        let work = &measured.threads[0];
        assert_eq!(work.interruptions[0].app, "Google Chrome");
        assert_eq!(work.interruptions[0].title, None);
    }

    #[test]
    fn the_bands_split_the_time_by_how_long_each_visit_held() {
        let episodes = sequence(vec![
            visit(0, "A", 5),
            visit(1, "B", 30),
            visit(2, "C", 300),
        ]);
        let measured = measure_all(&episodes);

        assert_eq!(measured.passing_ms, 5_000);
        assert_eq!(measured.brief_ms, 30_000);
        assert_eq!(measured.settled_ms, 300_000);
        assert_eq!(measured.switches, 2);
    }

    /// Too little to have a shape. Numbers drawn from four visits invite a reader to
    /// find one anyway.
    #[test]
    fn a_window_with_almost_nothing_in_it_reports_no_shape() {
        let episodes = sequence(vec![visit(0, "A", 20), visit(1, "B", 20)]);
        assert!(!measure_all(&episodes).is_meaningful());
    }

    /// The evening is a thing people ask about by name, and it is sliced by the hour a
    /// visit began in.
    #[test]
    fn a_range_of_hours_takes_what_began_inside_it() {
        let mut episodes = sequence(vec![visit(0, "A", 60), visit(1, "B", 60)]);
        let morning = Utc.with_ymd_and_hms(2026, 8, 27, 3, 0, 0).unwrap();
        episodes[0].start = stamp(morning);
        episodes[0].end = stamp(morning + Duration::seconds(60));

        // 19:00 UTC is not 19:00 anywhere but UTC, so the filter is exercised against
        // whatever the local hour of each stamp actually is rather than a fixed pair.
        let kept = between_hours(&episodes, 0, 23);
        assert_eq!(kept.len(), 2);
    }
}
