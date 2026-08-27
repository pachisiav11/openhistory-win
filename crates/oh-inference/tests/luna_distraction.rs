//! Asking Luna how scattered an evening was, against the machine's own history.
//!
//! Standalone and ignored by default. It is the only test in the tree that sends a
//! real request to a real provider, so a plain `cargo test` must never reach it: it
//! costs money and needs a key. Run it deliberately.
//!
//! ```text
//! cargo test -p oh-inference --test luna_distraction -- --ignored --nocapture
//! ```
//!
//! What it exercises is the thing the automated suite cannot: whether a model given
//! the measured attention block writes something a person recognises as their own
//! evening. That is a judgement, not an assertion, so the test writes its answers to
//! `drafts/luna-distraction/` and leaves the judging to a reader.
//!
//! The key is read from the Windows Credential Manager, exactly as the application
//! reads it. Nothing here writes a key anywhere, and no key is in this repository.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, Timelike, Utc};
use oh_inference::openai::OpenAiProvider;
use oh_inference::prompt::{CHAT_SYSTEM, Prompt, chat_prompt};
use oh_inference::provider::Request;
use oh_inference::secrets::{self, Secret};
use oh_processing::attention;
use oh_processing::rollup::{human_duration, roll_up};
use oh_processing::{DayReport, Episode};

/// The day the question is about. The user's own evening, as recorded.
const DAY: &str = "2026-08-27";

/// Where the evening starts. Five in the afternoon is what a person means by it.
const EVENING_FROM: u32 = 17;
const EVENING_TO: u32 = 23;

/// The question, as it was asked.
const QUESTION: &str = "How distracted was I while working on the critical in the evening today?";

const MODEL: &str = "gpt-5.6-luna";

/// One way of asking, and what it is for.
struct Variant {
    slug: &'static str,
    /// What this framing is trying to get that the others might not.
    intent: &'static str,
    /// Appended to the question. Empty for the control.
    steer: &'static str,
    max_tokens: u32,
    /// Whether to send the whole day rather than the evening alone.
    whole_day: bool,
}

/// Five framings over one set of measurements.
///
/// They share the system prompt, the data and the question, so what separates the
/// answers is the framing alone. The first is the control: it is exactly what the
/// application will send, and if it wins there is nothing further to change.
const VARIANTS: &[Variant] = &[
    Variant {
        slug: "1-as-shipped",
        intent: "Exactly what the app sends today. The control.",
        steer: "",
        max_tokens: 800,
        whole_day: false,
    },
    Variant {
        slug: "2-verdict-first",
        intent: "Answer in the first sentence, then the evidence behind it.",
        steer: "Answer in the first sentence, plainly, and spend the rest saying what in \
the log supports it.",
        max_tokens: 800,
        whole_day: false,
    },
    Variant {
        slug: "3-two-readings",
        intent: "Force the coupled-versus-displacing distinction into the open.",
        steer: "There are two readings of heavy switching: windows that were trading the \
foreground because they were halves of one task, and windows that were taking the \
foreground away from it. Say which of the two the evening's switching was, name the \
windows on each side, and say what in the log decides it.",
        max_tokens: 900,
        whole_day: false,
    },
    Variant {
        slug: "4-long-form",
        intent: "Room to reason. Tests whether more length buys more insight or more padding.",
        steer: "Take about four hundred words. Go through the evening in order, and end \
by saying what the shape of it suggests about how the work went.",
        max_tokens: 1_400,
        whole_day: false,
    },
    Variant {
        slug: "5-whole-day",
        intent: "The same question with the whole day in view, to see whether the evening \
reads differently against what came before it.",
        steer: "Answer about the evening, but say how it compares with the rest of the day \
where the log supports a comparison.",
        max_tokens: 900,
        whole_day: true,
    },
];

#[tokio::test]
#[ignore = "sends a real request to OpenAI and needs a key in the Credential Manager"]
async fn luna_answers_how_distracted_the_evening_was() {
    let date = NaiveDate::parse_from_str(DAY, "%Y-%m-%d").expect("a date");
    let day = load_day(date);
    let evening = evening_report(&day);

    let key = secrets::load(Secret::OpenAiApiKey)
        .expect("the credential store is readable")
        .expect("an OpenAI key is stored; set one in Settings first");

    let out = drafts_dir();
    fs::create_dir_all(&out).expect("the drafts directory");

    let mut written = Vec::new();
    for variant in VARIANTS {
        let report = if variant.whole_day { &day } else { &evening };
        let prompt = build(report, variant);

        eprintln!("--- {} ---", variant.slug);
        let provider = OpenAiProvider::new(key.clone(), MODEL).expect("a provider");
        let request = Request {
            prompt: prompt.clone(),
            timeout: Duration::from_secs(180),
        };

        let answer = match provider.complete(&request).await {
            Ok(completion) => completion.text,
            Err(error) => {
                eprintln!("{} failed: {error}", variant.slug);
                format!("*This variant failed: {error}*")
            }
        };
        eprintln!("{answer}\n");

        let path = out.join(format!("{}.md", variant.slug));
        fs::write(&path, page(variant, &answer)).expect("the draft is written");
        written.push(path);
    }

    // The prompt is the same for every variant but the steer, so one copy is enough to
    // see what the model was actually given.
    let prompt = build(&evening, &VARIANTS[0]);
    fs::write(out.join("00-the-prompt.md"), prompt_page(&prompt)).expect("the prompt is written");

    eprintln!("{} drafts written to {}", written.len(), out.display());
}

/// What the evening measured, with no model involved.
///
/// Separate from the asking so it can be run on its own: it needs no key and costs
/// nothing, and it is the fastest way to see whether the threading is finding the work.
#[tokio::test]
#[ignore = "reads the machine's own history, which CI does not have"]
async fn the_evening_measures_as_expected() {
    let date = NaiveDate::parse_from_str(DAY, "%Y-%m-%d").expect("a date");
    let day = load_day(date);
    let evening = evening_report(&day);
    let borrowed: Vec<&Episode> = evening.episodes.iter().collect();
    let measured = attention::measure(&borrowed);

    eprintln!(
        "{} episodes, {} active, {} switches, {:.0} an hour",
        measured.visits,
        human_duration(measured.active_ms),
        measured.switches,
        measured.switches_per_hour()
    );
    eprintln!(
        "  bands: {} passing, {} brief, {} settled",
        human_duration(measured.passing_ms),
        human_duration(measured.brief_ms),
        human_duration(measured.settled_ms)
    );
    eprintln!(
        "  {:.0}% of the time is inside a named stretch",
        measured.threaded_share() * 100.0
    );
    eprintln!("  {} stretches:", measured.threads.len());
    for thread in &measured.threads {
        eprintln!(
            "    {} {} — {} ({} crossings, {} visits, broken into {})",
            clock(&thread.start),
            thread.apps.join(" + "),
            human_duration(thread.active_ms),
            thread.crossings,
            thread.visits,
            thread.interruption_count()
        );
    }

    assert!(
        !measured.threads.is_empty(),
        "an evening of real work must produce at least one named stretch"
    );
}

/// The day's stored report, as the application would load it.
fn load_day(date: NaiveDate) -> DayReport {
    let path = oh_core::paths::episodes_dir()
        .expect("a data directory")
        .join(format!("{}.json", date.format("%Y-%m-%d")));
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} could not be read: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} is not a day report: {error}", path.display()))
}

/// The evening alone, rolled up as a day of its own.
///
/// The rollup has to be rebuilt rather than reused: the stored one is the whole day's,
/// and handing a model the day's totals beside the evening's episodes would have it
/// reasoning about hours that are not in front of it.
fn evening_report(day: &DayReport) -> DayReport {
    let episodes: Vec<Episode> = attention::between_hours(&day.episodes, EVENING_FROM, EVENING_TO)
        .into_iter()
        .cloned()
        .collect();
    let date = NaiveDate::parse_from_str(&day.date, "%Y-%m-%d").expect("a date");
    DayReport {
        date: day.date.clone(),
        rollup: roll_up(date, &episodes),
        episodes,
    }
}

/// The question this variant puts, steer and all.
fn asked(variant: &Variant) -> String {
    if variant.steer.is_empty() {
        QUESTION.to_owned()
    } else {
        format!("{QUESTION}\n\n{}", variant.steer)
    }
}

fn build(report: &DayReport, variant: &Variant) -> Prompt {
    let date = NaiveDate::parse_from_str(&report.date, "%Y-%m-%d").expect("a date");
    let question = asked(variant);
    let mut prompt = chat_prompt(
        date,
        report,
        &oh_core::DaySummary::new(date),
        &[],
        &question,
    );
    prompt.max_tokens = variant.max_tokens;
    prompt
}

/// One draft, with enough around it to judge it by.
fn page(variant: &Variant, answer: &str) -> String {
    format!(
        "# Draft {}\n\n\
**What this framing is for.** {}\n\n\
**Model.** `{MODEL}`, {} output tokens, {}.\n\n\
---\n\n\
{}\n\n\
---\n\n\
## What was asked\n\n> {}\n",
        variant.slug,
        variant.intent,
        variant.max_tokens,
        if variant.whole_day {
            "the whole day in view"
        } else {
            "the evening only"
        },
        answer.trim(),
        asked(variant).trim().replace('\n', "\n> "),
    )
}

/// The prompt itself, so a reader can check the answers against what was sent.
fn prompt_page(prompt: &Prompt) -> String {
    format!(
        "# What Luna was given\n\n\
The evening of {DAY}, from {EVENING_FROM}:00. This is the prompt behind draft 1; the \
others differ only in the steer added to the question.\n\n\
## System\n\n```\n{}\n```\n\n## User\n\n```\n{}\n```\n",
        CHAT_SYSTEM, prompt.user
    )
}

fn drafts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("drafts")
        .join("luna-distraction")
}

fn clock(stamp: &str) -> String {
    DateTime::parse_from_rfc3339(stamp)
        .map(|when| when.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_else(|_| "--:--".to_owned())
}

/// Kept so an unused-import warning does not hide a real one.
#[allow(dead_code)]
fn _hour_of(stamp: &str) -> Option<u32> {
    DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|when| when.with_timezone(&Utc).with_timezone(&Local).hour())
}
