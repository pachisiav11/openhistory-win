//! Turning a measured day into something a model can read.
//!
//! Two prompts exist: one for an hour, built from the episodes that overlapped it, and
//! one for a day, built from the hourly summaries already written. The day prompt
//! deliberately reads the hours rather than the episodes — a whole day of episodes is
//! far more than a small local model can hold, and summarizing summaries is what the
//! two-level shape is for.
//!
//! Everything that goes into a prompt passes through
//! [`oh_processing::redact::PublicEpisode`] first. That is the only path in, so an
//! executable path or a query string cannot reach a provider by accident.

use chrono::{DateTime, Local, NaiveDate, Timelike};
use oh_core::HourSummary;
use oh_processing::redact::PublicEpisode;
use oh_processing::rollup::{DailyRollup, HourlyRollup, human_duration};
use oh_processing::{DayReport, Episode};

/// The instruction both prompts share.
pub const SYSTEM: &str = "You summarize a person's computer activity from a log of \
which applications and windows they had in front of them. Be concrete: name the \
applications, files, and topics you can see. Never invent detail that is not in the \
log. Some entries are marked as private sessions with nothing recorded — mention them \
only as time spent in that application, and never guess what they contained. Reply \
with prose only: no headings, no bullet points, no preamble such as \"Here is\". Every \
sentence must carry something from the log: a name, a file, a page, a topic, a time or \
a duration. Do not characterize the time in general terms — no \"a productive \
session\", no \"a mix of tasks\" — and do not restate totals you were given. Window \
controls, menu and toolbar labels, and other interface furniture are not activity: \
never describe them. Where an entry carries nothing but an application name, say that \
the application was in use and nothing further — a short summary is the correct answer \
to a thin hour, and inventing detail to fill one is worse than brevity. Anything that \
held under a minute is a window that was touched rather than work that was done: leave \
it out, and never give it a duration it did not have. Call the work what the files and \
pages call it, not what you suppose it was for: reasoning about how the time was spent \
is wanted, but a purpose, a mood or an urgency the log does not evidence — \"homework\", \
\"urgent\", \"exploratory\" — is invention like any other.";

/// The most episodes to put in one hourly prompt. An hour with more than this was
/// spent switching windows, and the tail of the list adds noise rather than meaning.
const MAX_EPISODES_PER_HOUR: usize = 25;

/// The least time an episode must hold to be worth naming.
///
/// Below a minute it is a window that was touched, not work that was done. Ten seconds
/// in Windows Terminal came back as "five minutes of command-line activity" in a real
/// summary: the entry sat in the list looking like every other entry, so the model gave
/// it a sentence and a duration to match. Filtering here rather than asking the model to
/// use its judgement is the difference between a rule and a hope.
///
/// The hour's own total is untouched, so the time is still counted — it is only not
/// given a name of its own.
const MIN_EPISODE_MS: i64 = 60_000;

/// What was asked of a model, ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub system: String,
    pub user: String,
    /// Generous enough for the requested length and no more. A summary that runs long
    /// is a worse summary.
    pub max_tokens: u32,
}

/// Episodes that overlapped one local hour, in the order they started.
pub fn episodes_in_hour<'a>(report: &'a DayReport, hour: &HourlyRollup) -> Vec<&'a Episode> {
    hour.episode_ids
        .iter()
        .filter_map(|id| report.episodes.iter().find(|episode| &episode.id == id))
        .collect()
}

/// The prompt for one hour.
///
/// Returns `None` when the hour holds nothing worth describing. A model asked to
/// summarize thirty seconds of alt-tabbing writes something, and whatever it writes is
/// invention.
pub fn hour_prompt(date: NaiveDate, hour: &HourlyRollup, episodes: &[&Episode]) -> Option<Prompt> {
    if episodes.is_empty() || hour.active_ms < 60_000 {
        return None;
    }

    let mut user = format!(
        "Activity for {} between {:02}:00 and {:02}:59, {} of it active.\n\n",
        date.format("%A %-d %B %Y"),
        hour.hour,
        hour.hour,
        human_duration(hour.active_ms),
    );

    // An hour of nothing but brief switches still has to be describable, so the floor
    // is dropped rather than leaving the model an empty list to explain.
    let worth_naming: Vec<&Episode> = episodes
        .iter()
        .copied()
        .filter(|episode| episode.active_ms >= MIN_EPISODE_MS)
        .collect();
    let (named, brief) = if worth_naming.is_empty() {
        (episodes, 0)
    } else {
        let brief = episodes.len() - worth_naming.len();
        (worth_naming.as_slice(), brief)
    };

    for episode in named.iter().take(MAX_EPISODES_PER_HOUR) {
        user.push_str(&render_episode(&PublicEpisode::from(*episode), hour.hour));
    }
    if named.len() > MAX_EPISODES_PER_HOUR {
        user.push_str(&format!(
            "\n({} further short entries omitted.)\n",
            named.len() - MAX_EPISODES_PER_HOUR
        ));
    }
    if brief > 0 {
        user.push_str(&format!(
            "\n({brief} briefer switches, each under a minute, are counted in the total \
             above but are not listed: they were not worked in.)\n"
        ));
    }

    user.push_str(
        "\nThe entries above are this hour's activity, including any that began before \
it or ran past it. Write two or three sentences describing what was being worked on in \
this hour.",
    );

    Some(Prompt {
        system: SYSTEM.to_owned(),
        user,
        max_tokens: 300,
    })
}

/// The prompt for a whole day, built from the hours already summarized.
///
/// Returns `None` when no hour has been summarized: there is nothing to condense, and
/// a day summary derived from the rollup alone would only restate the numbers the
/// interface already shows.
pub fn day_prompt(date: NaiveDate, rollup: &DailyRollup, hours: &[HourSummary]) -> Option<Prompt> {
    if hours.is_empty() {
        return None;
    }

    let mut user = format!(
        "Hour-by-hour summaries for {}, {} active in total across {} sessions.\n\n",
        date.format("%A %-d %B %Y"),
        human_duration(rollup.active_ms),
        rollup.episodes,
    );

    for hour in hours {
        user.push_str(&format!(
            "{:02}:00 ({}) — {}\n",
            hour.hour,
            human_duration(hour.active_ms),
            hour.text.trim()
        ));
    }

    if !rollup.apps.is_empty() {
        let leaders: Vec<String> = rollup
            .apps
            .iter()
            .take(5)
            .map(|usage| format!("{} ({})", usage.app, human_duration(usage.active_ms)))
            .collect();
        user.push_str(&format!("\nTime by application: {}.\n", leaders.join(", ")));
    }
    if rollup.private_episodes > 0 {
        user.push_str(&format!(
            "{} private sessions were recorded as time only.\n",
            rollup.private_episodes
        ));
    }

    user.push_str(
        "\nWrite exactly three paragraphs, separated by blank lines, about 200 words in \
total.\n\n\
Paragraph one, about 50 words: what was worked on, naming the files, documents, pages \
and topics themselves.\n\n\
Paragraph two, about 100 words: analysis, not narration. Do not re-list what paragraph \
one already said. Say what the shape of the day means — where attention held and where \
it broke up, which pieces of work were competing for the same stretch of time, what the \
order they came in suggests was urgent as against merely open, and what was started and \
then abandoned. Draw conclusions the log supports but does not state outright, and say \
which reading the evidence favours where it is ambiguous.\n\n\
Paragraph three, about 50 words: begin with \"In conclusion\" and say what the day \
amounted to.",
    );

    Some(Prompt {
        system: SYSTEM.to_owned(),
        user,
        max_tokens: 500,
    })
}

/// The local clock time of a stored stamp, as the person sitting there read it.
fn local_clock(stamp: &str) -> String {
    match DateTime::parse_from_rfc3339(stamp) {
        Ok(when) => when.with_timezone(&Local).format("%H:%M").to_string(),
        Err(_) => "--:--".to_owned(),
    }
}

/// Which local hour a stored stamp falls in.
fn local_hour(stamp: &str) -> Option<u32> {
    DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|when| when.with_timezone(&Local).hour())
}

/// One episode as a line of the prompt.
///
/// Times are local, because the hour in the heading is local. Stamps are stored in UTC
/// and the clock used to be sliced straight out of the string, which put the heading and
/// the entries under it in different time zones: an hour headed 17:00 listed its entries
/// at 12:25. Handed that contradiction a model either reported the UTC times as though
/// they were the truth or refused the hour outright, saying the log did not cover the
/// window it had been asked about — which is what made the late hours of a day useless.
fn render_episode(episode: &PublicEpisode, hour: u32) -> String {
    let clock = local_clock(&episode.start);
    let spent = human_duration(episode.active_ms);

    // An episode is listed under every hour it overlapped, so one that began earlier or
    // ran on carries a start time outside this hour and an active total that is not all
    // this hour's. Both are said plainly; left bare they read as entries that do not
    // belong to the hour they are filed under.
    let when = if local_hour(&episode.start) != Some(hour) {
        format!("{clock}, began before this hour ({spent} in all)")
    } else if local_hour(&episode.end) != Some(hour) {
        format!("{clock}, runs past this hour ({spent} in all)")
    } else {
        format!("{clock} ({spent})")
    };

    if episode.is_private {
        return format!(
            "- {when} {} — private session, nothing recorded\n",
            episode.app
        );
    }

    let mut line = match &episode.title {
        Some(title) => format!("- {when} {}: {title}\n", episode.app),
        None => format!("- {when} {}\n", episode.app),
    };
    if !episode.urls.is_empty() {
        line.push_str(&format!("    visited: {}\n", episode.urls.join(", ")));
    }
    if !episode.documents.is_empty() {
        line.push_str(&format!("    document: {}\n", episode.documents.join(", ")));
    }
    if !episode.visible_text.is_empty() {
        // Labelled for what it is. A model told only "Preview, Outline, Retention
        // policy" will write them into a sentence as though they were done; told they
        // are what the window showed, it uses them to say what was being looked at.
        line.push_str(&format!(
            "    on screen: {}\n",
            episode.visible_text.join(" · ")
        ));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use oh_processing::rollup::AppUsage;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 22).unwrap()
    }

    /// A stored stamp — which is UTC — for a local wall-clock time on the test's date.
    ///
    /// Built through the local zone rather than written as a UTC literal so these tests
    /// mean the same thing wherever they run. The prompt files an episode by the local
    /// hour it falls in, so a fixed literal lands in a different hour on every machine,
    /// and a test written that way passes in London and fails in Delhi.
    fn at(hour: u32, minute: u32) -> String {
        let naive = date().and_hms_opt(hour, minute, 0).unwrap();
        Local
            .from_local_datetime(&naive)
            .earliest()
            .expect("a wall-clock time the local zone actually has")
            .with_timezone(&Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn episode(app: &str, title: Option<&str>, active_ms: i64) -> Episode {
        Episode {
            id: format!("2026-08-22#{app}"),
            date: "2026-08-22".into(),
            app: app.into(),
            app_path: Some(format!(r"C:\Users\someone\Apps\{app}.exe")),
            title: title.map(str::to_owned),
            titles: Vec::new(),
            urls: Vec::new(),
            documents: Vec::new(),
            visible_text: Vec::new(),
            start: at(9, 5),
            end: at(9, 35),
            duration_ms: 1_800_000,
            active_ms,
            event_count: 4,
            is_private: false,
        }
    }

    fn hour(active_ms: i64, ids: &[&str]) -> HourlyRollup {
        HourlyRollup {
            hour: 9,
            active_ms,
            apps: vec![AppUsage {
                app: "Visual Studio Code".into(),
                active_ms,
                episodes: ids.len(),
            }],
            episode_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    #[test]
    fn an_hour_prompt_names_the_applications_and_the_time() {
        let one = episode("Visual Studio Code", Some("collector.rs"), 900_000);
        let prompt = hour_prompt(date(), &hour(900_000, &[&one.id]), &[&one]).unwrap();

        assert!(prompt.user.contains("Visual Studio Code"));
        assert!(prompt.user.contains("collector.rs"));
        assert!(prompt.user.contains("09:00 and 09:59"));
        assert!(prompt.user.contains("Saturday 22 August 2026"));
        assert_eq!(prompt.system, SYSTEM);
    }

    #[test]
    fn an_executable_path_never_reaches_the_prompt() {
        let one = episode("Visual Studio Code", Some("collector.rs"), 900_000);
        let prompt = hour_prompt(date(), &hour(900_000, &[&one.id]), &[&one]).unwrap();

        assert!(!prompt.user.contains(".exe"), "{}", prompt.user);
        assert!(!prompt.user.contains(r"C:\Users"), "{}", prompt.user);
    }

    #[test]
    fn a_query_string_never_reaches_the_prompt() {
        let mut one = episode("Google Chrome", Some("Search"), 600_000);
        one.urls = vec!["https://example.com/search?q=something+private".into()];
        let prompt = hour_prompt(date(), &hour(600_000, &[&one.id]), &[&one]).unwrap();

        assert!(prompt.user.contains("https://example.com/search"));
        assert!(
            !prompt.user.contains("something+private"),
            "{}",
            prompt.user
        );
    }

    #[test]
    fn the_document_and_what_the_window_showed_reach_the_prompt() {
        let mut one = episode("Microsoft Word", Some("Document1 - Word"), 900_000);
        one.documents = vec!["quarterly-review.docx".into()];
        one.visible_text = vec!["Retention policy".into(), "Section 4".into()];
        let prompt = hour_prompt(date(), &hour(900_000, &[&one.id]), &[&one]).unwrap();

        assert!(prompt.user.contains("document: quarterly-review.docx"));
        assert!(
            prompt
                .user
                .contains("on screen: Retention policy · Section 4")
        );
    }

    #[test]
    fn a_private_session_is_named_as_time_only() {
        let mut one = episode("Google Chrome", Some("A secret page"), 600_000);
        one.is_private = true;
        let prompt = hour_prompt(date(), &hour(600_000, &[&one.id]), &[&one]).unwrap();

        assert!(prompt.user.contains("private session, nothing recorded"));
        assert!(!prompt.user.contains("A secret page"), "{}", prompt.user);
        assert!(prompt.system.contains("never guess what they contained"));
    }

    /// The defect this guards against made the late hours of a real day unusable. The
    /// heading is built from the local hour and the entries were sliced out of the UTC
    /// stamp, so an hour headed 20:00 listed entries at 15:04 and the model answered
    /// "these fall outside the requested time window" instead of summarizing it.
    #[test]
    fn the_entries_are_clocked_in_the_same_zone_as_the_heading() {
        let one = episode("Visual Studio Code", Some("collector.rs"), 900_000);
        let prompt = hour_prompt(date(), &hour(900_000, &[&one.id]), &[&one]).unwrap();

        assert!(prompt.user.contains("09:00 and 09:59"), "{}", prompt.user);
        assert!(prompt.user.contains("- 09:05 "), "{}", prompt.user);
    }

    #[test]
    fn an_episode_that_outlasts_the_hour_says_so_rather_than_looking_misfiled() {
        // Runs 09:45 to 10:20, so the 09:00 hour holds only part of it.
        let mut one = episode("Google Chrome", Some("problems.pdf"), 2_100_000);
        one.start = at(9, 45);
        one.end = at(10, 20);
        let prompt = hour_prompt(date(), &hour(900_000, &[&one.id]), &[&one]).unwrap();
        assert!(
            prompt.user.contains("runs past this hour"),
            "{}",
            prompt.user
        );

        // And the same episode seen from the 10:00 hour it ran into.
        let mut later = hour(900_000, &[&one.id]);
        later.hour = 10;
        let prompt = hour_prompt(date(), &later, &[&one]).unwrap();
        assert!(
            prompt.user.contains("began before this hour"),
            "{}",
            prompt.user
        );
    }

    /// Ten seconds in a terminal came back from a real run as "five minutes of
    /// command-line activity": the entry looked like every other entry, so it got a
    /// sentence and a duration to match. It is counted in the hour's total and not
    /// given a name of its own.
    #[test]
    fn a_window_touched_for_seconds_is_not_named() {
        let worked = episode("Microsoft Word", Some("final crit"), 900_000);
        let touched = episode("Windows Terminal", Some("cmd"), 10_000);
        let ids = [worked.id.as_str(), touched.id.as_str()];
        let prompt = hour_prompt(date(), &hour(910_000, &ids), &[&worked, &touched]).unwrap();

        assert!(prompt.user.contains("final crit"), "{}", prompt.user);
        assert!(!prompt.user.contains("Windows Terminal"), "{}", prompt.user);
        assert!(
            prompt.user.contains("1 briefer switches"),
            "{}",
            prompt.user
        );
    }

    /// An hour that was nothing but brief switches still has to be describable, so the
    /// floor gives way rather than handing the model an empty list to account for.
    #[test]
    fn an_hour_of_nothing_but_brief_switches_still_lists_them() {
        let one = episode("Google Chrome", Some("a tab"), 20_000);
        let two = episode("Slack", Some("a channel"), 20_000);
        let ids = [one.id.as_str(), two.id.as_str()];
        let prompt = hour_prompt(date(), &hour(120_000, &ids), &[&one, &two]).unwrap();

        assert!(prompt.user.contains("Google Chrome"), "{}", prompt.user);
        assert!(prompt.user.contains("Slack"), "{}", prompt.user);
        assert!(!prompt.user.contains("briefer switches"), "{}", prompt.user);
    }

    #[test]
    fn an_hour_with_almost_no_activity_is_not_worth_a_prompt() {
        let one = episode("Explorer", None, 20_000);
        assert!(hour_prompt(date(), &hour(20_000, &[&one.id]), &[&one]).is_none());
        assert!(hour_prompt(date(), &hour(900_000, &[]), &[]).is_none());
    }

    #[test]
    fn a_very_busy_hour_is_truncated_and_says_so() {
        let episodes: Vec<Episode> = (0..40)
            .map(|n| episode(&format!("App{n}"), Some("something"), 30_000))
            .collect();
        let refs: Vec<&Episode> = episodes.iter().collect();
        let ids: Vec<&str> = episodes.iter().map(|e| e.id.as_str()).collect();

        let prompt = hour_prompt(date(), &hour(1_200_000, &ids), &refs).unwrap();
        assert!(prompt.user.contains("15 further short entries omitted"));
        assert!(prompt.user.contains("App0"));
        assert!(!prompt.user.contains("App39"));
    }

    fn written(hour: u32, text: &str) -> HourSummary {
        HourSummary {
            hour,
            text: text.into(),
            active_ms: 1_800_000,
            generated_at: "2026-08-22T12:00:00.000Z".into(),
            provider: "local".into(),
            model: "test".into(),
        }
    }

    fn rollup() -> DailyRollup {
        DailyRollup {
            date: "2026-08-22".into(),
            active_ms: 3_600_000,
            idle_ms: 0,
            episodes: 12,
            apps: vec![AppUsage {
                app: "Visual Studio Code".into(),
                active_ms: 2_400_000,
                episodes: 6,
            }],
            hours: Vec::new(),
            first_activity: None,
            last_activity: None,
            private_episodes: 2,
        }
    }

    #[test]
    fn a_day_prompt_condenses_the_hours_that_were_written() {
        let hours = [
            written(9, "Worked on the collector."),
            written(10, "Reviewed a pull request."),
        ];
        let prompt = day_prompt(date(), &rollup(), &hours).unwrap();

        assert!(prompt.user.contains("Worked on the collector."));
        assert!(prompt.user.contains("Reviewed a pull request."));
        assert!(
            prompt
                .user
                .contains("Time by application: Visual Studio Code")
        );
        assert!(prompt.user.contains("2 private sessions"));
        assert!(prompt.max_tokens > 300);
    }

    #[test]
    fn a_day_with_no_hours_written_gets_no_prompt() {
        assert!(day_prompt(date(), &rollup(), &[]).is_none());
    }
}
