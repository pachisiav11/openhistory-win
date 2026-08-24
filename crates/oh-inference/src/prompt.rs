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

use chrono::NaiveDate;
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
session\", no \"a mix of tasks\" — and do not restate totals you were given.";

/// The most episodes to put in one hourly prompt. An hour with more than this was
/// spent switching windows, and the tail of the list adds noise rather than meaning.
const MAX_EPISODES_PER_HOUR: usize = 25;

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

    for episode in episodes.iter().take(MAX_EPISODES_PER_HOUR) {
        user.push_str(&render_episode(&PublicEpisode::from(*episode)));
    }
    if episodes.len() > MAX_EPISODES_PER_HOUR {
        user.push_str(&format!(
            "\n({} further short entries omitted.)\n",
            episodes.len() - MAX_EPISODES_PER_HOUR
        ));
    }

    user.push_str(
        "\nWrite two or three sentences describing what was being worked on in this hour.",
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
        "\nWrite an account of this day in three paragraphs, then a fourth. Target \
about 300 words in total — that is a target to stay close to, not a floor to fill. \
First: the pieces of work themselves, each one named — the files, documents, pages \
and topics it involved, and roughly how long each took. Second: the order the day \
ran in and where the work changed direction. Third: what the shape of the time says \
— the longest unbroken stretch and anything begun and not returned to. Name \
something concrete from the log in every sentence, but name each file, topic or \
figure once; do not return to something already covered. Where an hour holds \
little, write less about it rather than padding it out.\n\n\
After those three paragraphs, leave a blank line and add a fourth, separate from \
and outside the detailed account above, within the same 300-word target: two or \
three sentences that just summarize what was done during the day.",
    );

    Some(Prompt {
        system: SYSTEM.to_owned(),
        user,
        max_tokens: 700,
    })
}

/// One episode as a line of the prompt.
fn render_episode(episode: &PublicEpisode) -> String {
    let clock = episode.start.get(11..16).unwrap_or("--:--").to_owned();
    let spent = human_duration(episode.active_ms);

    if episode.is_private {
        return format!(
            "- {clock} ({spent}) {} — private session, nothing recorded\n",
            episode.app
        );
    }

    let mut line = match &episode.title {
        Some(title) => format!("- {clock} ({spent}) {}: {title}\n", episode.app),
        None => format!("- {clock} ({spent}) {}\n", episode.app),
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
    use oh_processing::rollup::AppUsage;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 22).unwrap()
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
            start: "2026-08-22T09:05:00.000Z".into(),
            end: "2026-08-22T09:35:00.000Z".into(),
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
