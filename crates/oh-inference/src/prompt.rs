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

use std::collections::BTreeMap;

use chrono::{DateTime, Local, NaiveDate, Timelike};
use oh_core::{DaySummary, HourSummary};
use oh_processing::attention::{self, Attention, Thread};
use oh_processing::redact::PublicEpisode;
use oh_processing::rollup::{DailyRollup, HourlyRollup, human_duration};
use oh_processing::{DayReport, Episode};
use serde::{Deserialize, Serialize};

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
to a thin hour, and inventing detail to fill one is worse than brevity. Call the work \
what the files and pages call it, not what you suppose it was for: reasoning about how \
the time was spent is wanted, but a purpose, a mood or an urgency the log does not \
evidence — \"homework\", \"urgent\", \"exploratory\" — is invention like any other. \
Where an entry reports what was on screen, that is evidence like any other and you \
should use it: say what a document, page or conversation was about from the words it \
was showing, rather than only repeating what it was named. A file whose name is an \
abbreviation and whose text is an essay about one subject is an essay about that \
subject, and saying so is reading the log rather than guessing at it — but the subject \
must come from the words recorded, never from what a name like that usually means.\n\n\
Each entry you are given is one continuous stretch of work rather than one visit to a \
window, and its time is the whole stretch's. Do not treat a stretch as brief because \
the visits inside it were: work carried across a pair of windows is made of short \
visits by construction, and the entry's own duration is the one to use. Where an entry \
names two windows, that is one piece of work carried across both — a document and the \
source it is being written from, an editor and the terminal that runs it — and \
crossing between such a pair, however many times, is not interruption and must not be \
described as any. What the log lists separately, as having broken into a stretch, is \
the interruption: name it, say how often it happened, and say that the work resumed. \
Moving between windows has been counted for you; use the counts you are given, quote \
them where they matter, and never estimate one of your own. A count of switches is not \
distraction on its own — the evidence that tells the two apart is which windows were \
trading the foreground and whether the work resumed. Where both readings are open, \
give the one the evidence favours and say what favours it. Report what the switching \
shows, including when it shows a stretch that held; do not deliver a verdict on the \
person.";

/// The most episodes to put in one hourly prompt. An hour with more than this was
/// spent switching windows, and the tail of the list adds noise rather than meaning.
const MAX_EPISODES_PER_HOUR: usize = 25;

/// The most threads laid out for one day in a chat prompt.
///
/// A thread is a whole piece of work rather than a visit, so a day runs to a couple of
/// dozen of them even when it holds five hundred episodes. The cap is a backstop, not
/// a filter that anything real is expected to hit.
const MAX_CHAT_THREADS: usize = 40;

/// The most alternating pairs named in one attention block.
const MAX_NAMED_PAIRS: usize = 4;

/// The most documents, pages or lines of screen text carried on one thread.
///
/// A thread can gather a hundred visits, and the union of what they showed would be a
/// transcript. These bound the union, not the visit.
const MAX_THREAD_DOCUMENTS: usize = 6;
const MAX_THREAD_URLS: usize = 6;
const MAX_THREAD_TEXT: usize = 10;
const MAX_THREAD_TITLES: usize = 4;

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

    let attention = attention::measure(episodes);
    let mut threads = attention.threads.clone();
    threads.sort_by(|left, right| left.start.cmp(&right.start));

    if threads.is_empty() {
        // Nothing here lasted long enough to be a piece of work. The visits are laid
        // out as they are rather than leaving the model an empty list to explain, and
        // the attention block below says plainly that none of them held.
        for episode in episodes.iter().take(MAX_EPISODES_PER_HOUR) {
            user.push_str(&render_episode(&PublicEpisode::from(*episode), hour.hour));
        }
        if episodes.len() > MAX_EPISODES_PER_HOUR {
            user.push_str(&format!(
                "\n({} further entries omitted.)\n",
                episodes.len() - MAX_EPISODES_PER_HOUR
            ));
        }
    } else {
        let lookup = by_id(episodes);
        for thread in threads.iter().take(MAX_EPISODES_PER_HOUR) {
            user.push_str(&render_thread(thread, &lookup, Some(hour.hour)));
        }
        if threads.len() > MAX_EPISODES_PER_HOUR {
            user.push_str(&format!(
                "\n({} further stretches omitted.)\n",
                threads.len() - MAX_EPISODES_PER_HOUR
            ));
        }
        let loose = loose_visits(&attention, &threads);
        if loose > 0 {
            user.push_str(&format!(
                "\n({loose} further foreground visits belonged to no stretch that lasted \
                 a minute. Their time is in the total above; they are not listed.)\n"
            ));
        }
    }

    user.push_str(&render_attention(&attention, "this hour"));

    user.push_str(
        "\nThe entries above are this hour's activity, including any that began before \
it or ran past it. Write two or three sentences describing what was being worked on in \
this hour. Where an entry says what was on screen, use it to say what the document, page \
or conversation was actually about — the day summary is written from these sentences and \
cannot see the screen text itself, so a name left unexplained here stays unexplained.",
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
pub fn day_prompt(
    date: NaiveDate,
    rollup: &DailyRollup,
    hours: &[HourSummary],
    attention: &Attention,
) -> Option<Prompt> {
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

    // The hours above were each written without sight of the others, so no hourly
    // summary can say how the day switched between them. The measurements can, and
    // this is the only place they reach the day.
    user.push_str(&render_attention(attention, "this day"));
    if let Some(longest) = attention.threads.first() {
        user.push_str(&format!(
            "The longest single piece of work was {} in {}, beginning at {}{}.\n",
            human_duration(longest.active_ms),
            describe_apps(longest),
            local_clock(&longest.start),
            match longest.interruption_count() {
                0 => " and unbroken".to_owned(),
                broken => format!(", broken into {} and resumed each time", times(broken)),
            }
        ));
    }

    user.push_str(
        "\nWrite exactly four paragraphs, separated by blank lines, about 500 words in \
total.\n\n\
Paragraph one, about 50 words: what was worked on, naming the files, documents, pages \
and topics themselves.\n\n\
Paragraphs two and three are the analysis, not narration, and together are by far the \
longest part: about 200 words each, one continuous argument split in two for \
readability rather than two separate topics. Do not re-list what paragraph one already \
said, and do not let paragraph three repeat paragraph two.\n\n\
Paragraph two covers what the day's stretches actually were and how attention moved \
between them:\n\
- What the day's named things actually were. Where the hours say what a document, page \
or conversation was showing, use it: give the subject, not only the file name. A name \
nobody explained stays a name, and you should say so rather than supply a meaning for it.\n\
- Where attention held. Which stretches ran long and unbroken, and on what.\n\
- Where it broke up, and what it broke up into. If one piece of work was the day's \
nominal business and something else kept taking the foreground away from it, say so \
plainly: name both, say how often the switching happened and across which hours, and say \
whether the returns were long enough to be work in their own right or short enough to be \
interruption.\n\
- Whether what pulled attention away was serving the main work or displacing it. A \
reference consulted about the thing being written is not the same as a second task, and \
the log's timing and subjects are what tell the two apart. Say which reading the evidence \
favours and why.\n\
- The switching was counted for you above; use those numbers and never estimate your \
own. A pair of windows that traded the foreground many times is one piece of work \
carried across two windows, not two pieces competing for the day: say which of those \
the evidence supports before calling any of it distraction. A stretch made entirely of \
short visits can be the day's most sustained work, and the stretches listed above are \
what the time went into rather than the visits inside them.\n\n\
Paragraph three covers the day's shape across the hours as a whole:\n\
- Which pieces of work were competing for the same stretch of time, and what the order \
they came in suggests was urgent as against merely open.\n\
- What ran across several hours and what its returning suggests; what appears once and \
never again; what was started and then abandoned.\n\
- Which hours carried the day's weight and which were interruption or upkeep, and how the \
two were interleaved.\n\n\
Where two readings of a stretch are both open, give both and say which the evidence \
favours. Draw conclusions the log supports but does not state outright. Write each of the \
two as continuous prose: the lists above are what to cover, not a shape to reproduce.\n\n\
Paragraph four, about 50 words: begin with \"In conclusion\" and say what the day \
amounted to.",
    );

    Some(Prompt {
        system: SYSTEM.to_owned(),
        user,
        max_tokens: 1_200,
    })
}

/// The instruction for answering a question about a day rather than summarizing it.
///
/// It shares the summariser's discipline — name what is there, invent nothing, leave
/// interface furniture alone — and departs from it in the two ways a conversation
/// needs. A question can be answered in whatever length the answer takes rather than a
/// fixed one, and a question the log cannot answer has to be refused outright. A model
/// that will not say "the log does not record that" will guess instead, and a guess
/// about a person's own day is worse than a refusal because they cannot tell the two
/// apart.
pub const CHAT_SYSTEM: &str = "You answer questions about a person's own computer \
activity, from a log of which applications and windows they had in front of them. The \
log below is everything you know. Answer only from it. Where it does not carry the \
answer, say plainly that it does not record that, and stop — never fill the gap with \
what usually happens, what probably happened, or what would be reasonable. Be \
concrete: name the applications, files, pages and topics in the log, and give times \
and durations where they answer the question. Entries marked as private sessions are \
time in an application and nothing more; never guess what they contained. Window \
controls, menu and toolbar labels and other interface furniture are not activity, and \
are not evidence about one. Where an entry reports what was on screen, that is \
evidence like any other: use it to say what a document, page or conversation was \
about, from the words recorded and never from what a name like that usually means. Do \
not characterize the day in general terms — no \"a productive session\", no \"a mix of \
tasks\". Write prose, with no headings and no preamble. The person asking is the \
person the log is about, so write to them as \"you\".\n\n\
Open with a direct answer to the question actually asked, in one plain sentence, \
before any of the evidence — then spend the rest of the answer, at whatever length the \
question takes, on what in the log supports that opening sentence. A question about \
one moment wants a short answer and little evidence after it; a question about the \
shape of the day wants a longer one. The opening sentence is a reading of the log, not \
a judgement of the person: state it plainly, without softening it into a list of \
caveats and without praising or blaming — say what happened, not whether it was good.\n\n\
Questions about focus, distraction and switching are answered from the measurements in \
the log and from nothing else. The counts are given to you: use them, quote them, and \
never estimate a number of your own. Switching is not distraction by itself. A stretch \
of work is very often carried across a pair of windows — a document and the source it is \
being written from, an editor and the terminal that runs it — and crossing between such a \
pair, however many hundred times, is one piece of work and must be described as one. The \
log marks these stretches for you and counts the crossings inside them; what it lists \
separately, as an interruption, is a window that took the foreground away from a stretch \
and handed it back. Those are the two things a question about distraction is really \
asking you to tell apart, so tell them apart explicitly in the opening sentence itself: \
say plainly how distracted the person was, and then say what broke in, how often, and \
whether the work resumed. Where the evidence allows both readings, open with the one it \
favours and say why. A number alone is not an answer — a hundred crossings between a \
draft and its source is concentration, and six departures to something unrelated may \
not be — so the opening sentence must already reflect that distinction rather than \
leaving it for the evidence to sort out.";

/// One exchange already in the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    pub asked: String,
    pub answered: String,
}

/// The most exchanges carried back into a new question.
///
/// The day itself is the bulk of the prompt and it is sent again with every question,
/// so the history is what has to be bounded. Eight is more than a conversation about
/// one day has needed, and it still leaves the day room.
const MAX_CHAT_TURNS: usize = 8;

/// The most episodes laid out for one day.
///
/// A day of real work runs to a few dozen once the sub-minute switches are dropped.
/// The cap is what stops a day of constant alt-tabbing from crowding out the question.
const MAX_CHAT_EPISODES: usize = 120;

/// The prompt for one question about one day.
///
/// Unlike the day summary, which reads the hours because a day of episodes is more
/// than a small model can hold, this reads both. A question can be about a moment the
/// hourly summaries never named, and the episodes are the only place that moment
/// exists; the written summary is included as well, where there is one, so that an
/// answer does not contradict what the person is reading above it.
pub fn chat_prompt(
    date: NaiveDate,
    report: &DayReport,
    summary: &DaySummary,
    turns: &[ChatTurn],
    question: &str,
) -> Prompt {
    let rollup = &report.rollup;
    let mut user = format!(
        "Activity for {}, {} active in total across {} sessions.\n\n",
        date.format("%A %-d %B %Y"),
        human_duration(rollup.active_ms),
        rollup.episodes,
    );

    if let Some(daily) = &summary.daily {
        user.push_str(&format!(
            "The summary already written for this day:\n{}\n\n",
            daily.trim()
        ));
    }

    if !summary.hours.is_empty() {
        user.push_str("Hour by hour:\n");
        for hour in &summary.hours {
            user.push_str(&format!(
                "{:02}:00 ({}) — {}\n",
                hour.hour,
                human_duration(hour.active_ms),
                hour.text.trim()
            ));
        }
        user.push('\n');
    }

    let borrowed: Vec<&Episode> = report.episodes.iter().collect();
    let attention = attention::measure(&borrowed);
    let mut threads = attention.threads.clone();
    threads.sort_by(|left, right| left.start.cmp(&right.start));

    if threads.is_empty() {
        if borrowed.is_empty() {
            user.push_str("Nothing was recorded on this day.\n\n");
        } else {
            user.push_str(
                "No stretch of this day held one place for a minute. What was in front, \
in the order it happened:\n",
            );
            for episode in borrowed.iter().take(MAX_CHAT_EPISODES) {
                user.push_str(&render_episode_in_day(&PublicEpisode::from(*episode)));
            }
            if borrowed.len() > MAX_CHAT_EPISODES {
                user.push_str(&format!(
                    "\n({} further entries are not listed.)\n",
                    borrowed.len() - MAX_CHAT_EPISODES
                ));
            }
            user.push('\n');
        }
    } else {
        let lookup = by_id(&borrowed);
        user.push_str(
            "What was worked on, in the order it happened. Each entry is one continuous \
stretch of work, which may have been carried across a pair of windows: where it was, the \
crossings between them are counted rather than listed one by one, and they are crossings \
within the work rather than departures from it.\n",
        );
        for thread in threads.iter().take(MAX_CHAT_THREADS) {
            user.push_str(&render_thread(thread, &lookup, None));
        }
        if threads.len() > MAX_CHAT_THREADS {
            user.push_str(&format!(
                "\n({} further stretches are not listed.)\n",
                threads.len() - MAX_CHAT_THREADS
            ));
        }
        let loose = loose_visits(&attention, &threads);
        if loose > 0 {
            user.push_str(&format!(
                "\n({loose} further foreground visits belonged to no stretch that lasted \
                 a minute. Their time is in the total above; they are not listed.)\n"
            ));
        }
        user.push('\n');
    }

    user.push_str(&render_attention(&attention, "this day"));
    user.push('\n');

    if !rollup.apps.is_empty() {
        let leaders: Vec<String> = rollup
            .apps
            .iter()
            .take(8)
            .map(|usage| format!("{} ({})", usage.app, human_duration(usage.active_ms)))
            .collect();
        user.push_str(&format!("Time by application: {}.\n\n", leaders.join(", ")));
    }
    if rollup.private_episodes > 0 {
        user.push_str(&format!(
            "{} private sessions were recorded as time only.\n\n",
            rollup.private_episodes
        ));
    }

    // Only the tail is carried. The day above it is what the answer is drawn from, and
    // an old exchange is worth less than the room it takes.
    let carried = turns.len().saturating_sub(MAX_CHAT_TURNS);
    if carried < turns.len() {
        user.push_str("Earlier in this conversation:\n");
        for turn in &turns[carried..] {
            user.push_str(&format!(
                "They asked: {}\nYou answered: {}\n",
                turn.asked.trim(),
                turn.answered.trim()
            ));
        }
        user.push('\n');
    }

    user.push_str(&format!(
        "Their question: {}\n\nAnswer it from the log above. If the log does not record \
what they are asking about, say so rather than supplying an answer.",
        question.trim()
    ));

    Prompt {
        system: CHAT_SYSTEM.to_owned(),
        user,
        max_tokens: 800,
    }
}

/// One episode as a line in a whole day's list.
///
/// [`render_episode`] describes an episode relative to the hour it is filed under,
/// which is meaningless here: this list is the day in order, and every entry belongs
/// to it.
fn render_episode_in_day(episode: &PublicEpisode) -> String {
    let when = format!(
        "{} ({})",
        local_clock(&episode.start),
        human_duration(episode.active_ms)
    );

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
        line.push_str(&format!(
            "    on screen: {}\n",
            episode.visible_text.join(" · ")
        ));
    }
    line
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

/// The episodes of a window, reachable by the ids a thread carries.
fn by_id<'a>(episodes: &[&'a Episode]) -> BTreeMap<String, &'a Episode> {
    episodes
        .iter()
        .map(|episode| (episode.id.clone(), *episode))
        .collect()
}

/// Foreground visits that belonged to no stretch long enough to be named.
///
/// Their time is in every total; what they lack is a name of their own. Counted rather
/// than listed, because a hundred of them listed is the noise the threads exist to
/// remove.
fn loose_visits(attention: &Attention, threads: &[Thread]) -> usize {
    let listed: usize = threads
        .iter()
        .map(|thread| thread.visits + thread.interruption_count())
        .sum();
    attention.visits.saturating_sub(listed)
}

/// One continuous piece of work as a block of the prompt.
///
/// `hour` is the hour the block is filed under, when it is filed under one. A stretch
/// that began before that hour or ran past it says so, for the same reason an episode
/// does: left bare it reads as an entry that does not belong where it sits.
fn render_thread(
    thread: &Thread,
    lookup: &BTreeMap<String, &Episode>,
    hour: Option<u32>,
) -> String {
    let members: Vec<PublicEpisode> = thread
        .episode_ids
        .iter()
        .filter_map(|id| lookup.get(id))
        .map(|episode| PublicEpisode::from(*episode))
        .collect();

    let clock = local_clock(&thread.start);
    let spent = human_duration(thread.active_ms);
    let when = match hour {
        Some(hour) if local_hour(&thread.start) != Some(hour) => {
            format!("{clock}, began before this hour ({spent} in all)")
        }
        Some(hour) if local_hour(&thread.end) != Some(hour) => {
            format!("{clock}, runs past this hour ({spent} in all)")
        }
        _ => format!("{clock}\u{2013}{} ({spent})", local_clock(&thread.end)),
    };

    let mut line = format!("- {when} {}", describe_apps(thread));

    // The distance between the work and the stretch it took. Only worth saying when
    // there is a distance: for a single unbroken sitting the two are the same number,
    // and printing it twice invites a reader to find meaning in the repetition.
    if thread.span_ms >= thread.active_ms + 60_000 {
        line.push_str(&format!(
            ", in a stretch of {} from first to last",
            human_duration(thread.span_ms)
        ));
    }
    if thread.crossings > 0 {
        line.push_str(&format!(
            ", {} crossings between the two across {} visits",
            thread.crossings, thread.visits
        ));
    } else if thread.visits > 1 {
        line.push_str(&format!(", {} visits", thread.visits));
    }
    line.push('\n');

    if members.iter().all(|episode| episode.is_private) {
        return format!(
            "- {when} {} — private session, nothing recorded\n",
            describe_apps(thread)
        );
    }

    let titles = gather(&members, MAX_THREAD_TITLES, |episode| {
        episode.title.clone().into_iter().collect()
    });
    let documents = gather(&members, MAX_THREAD_DOCUMENTS, |episode| {
        episode.documents.clone()
    });
    let urls = gather(&members, MAX_THREAD_URLS, |episode| episode.urls.clone());
    let text = gather(&members, MAX_THREAD_TEXT, |episode| {
        episode.visible_text.clone()
    });

    if !titles.is_empty() {
        line.push_str(&format!("    window: {}\n", titles.join(" · ")));
    }
    if !documents.is_empty() {
        line.push_str(&format!("    document: {}\n", documents.join(", ")));
    }
    if !urls.is_empty() {
        line.push_str(&format!("    visited: {}\n", urls.join(", ")));
    }
    if !text.is_empty() {
        line.push_str(&format!("    on screen: {}\n", text.join(" · ")));
    }

    if !thread.interruptions.is_empty() {
        let broke: Vec<String> = thread
            .interruptions
            .iter()
            .map(|out| {
                let named = match &out.title {
                    Some(title) => format!("{} ({title})", out.app),
                    None => out.app.clone(),
                };
                format!(
                    "{named} {}, {}",
                    times(out.visits),
                    human_duration(out.active_ms)
                )
            })
            .collect();
        line.push_str(&format!(
            "    broken into {}, {} in all, and resumed each time: {}\n",
            times(thread.interruption_count()),
            human_duration(thread.interrupted_ms()),
            broke.join("; ")
        ));
        if let Some(mean) = thread.mean_uninterrupted_ms() {
            line.push_str(&format!(
                "    which left about {} of work between one interruption and the next\n",
                human_duration(mean)
            ));
        }
    }

    line
}

/// A count of occasions, in words that read as English.
///
/// "Broken into 1 times" is the kind of seam that makes a reader trust the rest of the
/// line less, and the rest of the line is measured to the second.
fn times(count: usize) -> String {
    match count {
        1 => "once".to_owned(),
        2 => "twice".to_owned(),
        many => format!("{many} times"),
    }
}

/// The windows a thread ran in, as a phrase.
fn describe_apps(thread: &Thread) -> String {
    match thread.apps.as_slice() {
        [only] => only.clone(),
        [lead, second, ..] => format!("{lead} with {second}"),
        [] => "an unnamed window".to_owned(),
    }
}

/// Everything the episodes of a thread showed, in first-seen order, without repeats.
fn gather(
    members: &[PublicEpisode],
    limit: usize,
    pick: impl Fn(&PublicEpisode) -> Vec<String>,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for episode in members {
        if episode.is_private {
            continue;
        }
        for value in pick(episode) {
            if seen.len() >= limit {
                return seen;
            }
            if !seen.iter().any(|held| held == &value) {
                seen.push(value);
            }
        }
    }
    seen
}

/// The measured shape of attention over a window, as facts the model may use.
///
/// Every number here is counted, not estimated. It is written out rather than left for
/// the model to derive because a model asked to count three hundred entries will
/// produce a number that looks like a count and is not one, and a wrong number about a
/// person's own day is worse than no number: they cannot tell the two apart.
fn render_attention(attention: &Attention, window: &str) -> String {
    if !attention.is_meaningful() {
        return String::new();
    }

    let mut block = format!(
        "\nHow attention moved in {window}, measured: {} foreground visits across {} \
applications, with {} handovers from one application to another — {:.0} an hour against \
active time. The average visit lasted {}.",
        attention.visits,
        attention.distinct_apps,
        attention.switches,
        attention.switches_per_hour(),
        human_duration(attention.mean_visit_ms()),
    );

    // The sentence that stops a low mean visit length being read as a verdict on its
    // own. Work carried across two windows is made of short visits by construction, and
    // a reader given only that number would call half an hour of writing a half hour of
    // distraction.
    block.push_str(&format!(
        " A short visit is not the same as a short piece of work: {:.0}% of the active \
time belonged to one of the stretches listed above, which are what the time was \
actually spent on.",
        attention.threaded_share() * 100.0,
    ));

    if !attention.pairs.is_empty() {
        let pairs: Vec<String> = attention
            .pairs
            .iter()
            .take(MAX_NAMED_PAIRS)
            .map(|pair| format!("{} with {} {}", pair.a, pair.b, times(pair.crossings)))
            .collect();
        block.push_str(&format!(
            " The windows that handed the foreground to each other most often: {}.",
            pairs.join(", ")
        ));
    }

    block.push('\n');
    block
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
        assert!(prompt.user.contains("- 09:05–"), "{}", prompt.user);
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
    fn a_window_touched_for_seconds_is_an_interruption_and_not_work() {
        // Between two halves of the same work, which is where an interruption
        // actually happens. One that arrives after the work has stopped is not an
        // interruption of it, and is not reported as one.
        let before = beat(0, "Microsoft Word", "final crit", 450);
        let touched = beat(1, "Windows Terminal", "cmd", 10);
        let after = beat(2, "Microsoft Word", "final crit", 450);
        let ids = [before.id.as_str(), touched.id.as_str(), after.id.as_str()];
        let prompt =
            hour_prompt(date(), &hour(910_000, &ids), &[&before, &touched, &after]).unwrap();

        assert!(prompt.user.contains("final crit"), "{}", prompt.user);
        // Named for what it was, with its own ten seconds, and never as a stretch of
        // work with a duration of its own. The old floor deleted it outright, which
        // left the hour looking cleaner than it was.
        assert!(prompt.user.contains("broken into once"), "{}", prompt.user);
        assert!(prompt.user.contains("Windows Terminal"), "{}", prompt.user);
        assert!(
            !prompt.user.contains("Windows Terminal, in a stretch"),
            "it is an interruption, not a stretch: {}",
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
        // Forty different applications, none paired with another, so each 30-second
        // visit is a one-window thread of its own: forty threads, truncated the same
        // way forty loose episodes used to be.
        let episodes: Vec<Episode> = (0..40)
            .map(|n| episode(&format!("App{n}"), Some("something"), 30_000))
            .collect();
        let refs: Vec<&Episode> = episodes.iter().collect();
        let ids: Vec<&str> = episodes.iter().map(|e| e.id.as_str()).collect();

        let prompt = hour_prompt(date(), &hour(1_200_000, &ids), &refs).unwrap();
        assert!(
            prompt.user.contains("15 further stretches omitted"),
            "{}",
            prompt.user
        );
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
        let prompt = day_prompt(date(), &rollup(), &hours, &Attention::default()).unwrap();

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
        assert!(day_prompt(date(), &rollup(), &[], &Attention::default()).is_none());
    }

    #[test]
    fn the_analysis_is_the_bulk_of_the_day_summary_split_in_two_for_readability() {
        let prompt = day_prompt(
            date(),
            &rollup(),
            &[written(9, "Worked.")],
            &Attention::default(),
        )
        .unwrap();

        assert!(prompt.user.contains("about 500 words in total"));
        assert!(prompt.user.contains("Write exactly four paragraphs"));
        assert!(
            prompt
                .user
                .contains("about 200 words each, one continuous argument split in two"),
            "{}",
            prompt.user
        );
        // Four hundred words of analysis will not fit in the budget an earlier, shorter
        // paragraph was given, and a summary cut off mid-sentence is worse than a
        // short one.
        assert!(prompt.max_tokens >= 1_200);
    }

    /// The day summary is written from the hourly text alone, so a document whose name
    /// says nothing stays unexplained unless both prompts ask for its subject.
    #[test]
    fn both_prompts_ask_what_the_named_things_were_actually_about() {
        let one = episode("Microsoft Word", Some("final crit"), 900_000);
        let hourly = hour_prompt(date(), &hour(900_000, &[&one.id]), &[&one]).unwrap();
        assert!(hourly.user.contains("actually about"), "{}", hourly.user);

        let daily = day_prompt(
            date(),
            &rollup(),
            &[written(9, "Worked.")],
            &Attention::default(),
        )
        .unwrap();
        assert!(
            daily
                .user
                .contains("give the subject, not only the file name")
        );
        assert!(SYSTEM.contains("from the words it was showing"));
    }

    /// What the user asked the analysis to notice: a day spent nominally on one thing
    /// and repeatedly interrupted by another.
    #[test]
    fn the_analysis_is_asked_about_attention_divided_between_two_things() {
        let prompt = day_prompt(
            date(),
            &rollup(),
            &[written(9, "Worked.")],
            &Attention::default(),
        )
        .unwrap();

        assert!(prompt.user.contains("nominal business"), "{}", prompt.user);
        assert!(prompt.user.contains("how often the switching happened"));
        assert!(
            prompt
                .user
                .contains("serving the main work or displacing it")
        );
    }

    fn day_report(episodes: Vec<Episode>) -> DayReport {
        DayReport {
            date: "2026-08-22".into(),
            episodes,
            rollup: rollup(),
        }
    }

    fn day_summary(daily: Option<&str>) -> DaySummary {
        DaySummary {
            date: "2026-08-22".into(),
            daily: daily.map(str::to_owned),
            daily_generated_at: None,
            hours: Vec::new(),
        }
    }

    #[test]
    fn a_chat_prompt_carries_the_day_and_the_question() {
        let report = day_report(vec![episode(
            "Visual Studio Code",
            Some("collector.rs"),
            900_000,
        )]);
        let prompt = chat_prompt(
            date(),
            &report,
            &day_summary(None),
            &[],
            "  What took the morning?  ",
        );

        assert_eq!(prompt.system, CHAT_SYSTEM);
        assert!(
            prompt.user.contains("Saturday 22 August 2026"),
            "{}",
            prompt.user
        );
        assert!(prompt.user.contains("collector.rs"), "{}", prompt.user);
        // Trimmed, and asked as the question rather than pasted in raw.
        assert!(
            prompt
                .user
                .contains("Their question: What took the morning?"),
            "{}",
            prompt.user
        );
    }

    /// The redaction is structural: every episode goes through `PublicEpisode`, so an
    /// executable path cannot reach a provider even from this new path in.
    #[test]
    fn a_chat_prompt_never_carries_an_executable_path() {
        let report = day_report(vec![episode(
            "Visual Studio Code",
            Some("collector.rs"),
            900_000,
        )]);
        let prompt = chat_prompt(date(), &report, &day_summary(None), &[], "What happened?");

        assert!(
            !prompt.user.contains(r"C:\Users\someone"),
            "{}",
            prompt.user
        );
    }

    /// The same floor the summaries use, so an answer and the list a person is reading
    /// beside it agree about what counted.
    #[test]
    fn a_chat_prompt_names_what_broke_in_without_calling_it_work() {
        let report = day_report(vec![
            beat(0, "Visual Studio Code", "collector.rs", 450),
            beat(1, "Calculator", "Calculator", 20),
            beat(2, "Visual Studio Code", "collector.rs", 450),
        ]);
        let prompt = chat_prompt(date(), &report, &day_summary(None), &[], "What happened?");

        assert!(prompt.user.contains("collector.rs"), "{}", prompt.user);
        assert!(
            prompt.user.contains("broken into once"),
            "what interrupted the work is part of the answer: {}",
            prompt.user
        );
        assert!(prompt.user.contains("Calculator"), "{}", prompt.user);
    }

    /// An answer that contradicted the summary on the same screen would be worse than
    /// no answer, so the summary goes in where there is one.
    #[test]
    fn a_written_summary_goes_in_with_the_question() {
        let report = day_report(vec![episode("Visual Studio Code", None, 900_000)]);
        let prompt = chat_prompt(
            date(),
            &report,
            &day_summary(Some("A morning of Rust.")),
            &[],
            "What happened?",
        );

        assert!(
            prompt.user.contains("A morning of Rust."),
            "{}",
            prompt.user
        );
    }

    #[test]
    fn only_the_last_few_exchanges_are_carried_back() {
        let report = day_report(vec![episode("Visual Studio Code", None, 900_000)]);
        let turns: Vec<ChatTurn> = (0..12)
            .map(|n| ChatTurn {
                asked: format!("question {n}"),
                answered: format!("answer {n}"),
            })
            .collect();

        let prompt = chat_prompt(date(), &report, &day_summary(None), &turns, "And now?");

        // Twelve asked, eight carried: the four oldest are dropped from the front.
        assert!(!prompt.user.contains("question 3"), "{}", prompt.user);
        assert!(prompt.user.contains("question 4"), "{}", prompt.user);
        assert!(prompt.user.contains("question 11"), "{}", prompt.user);
        assert!(prompt.user.contains("answer 11"), "{}", prompt.user);
    }

    #[test]
    fn a_day_with_nothing_in_it_says_so_rather_than_listing_nothing() {
        let prompt = chat_prompt(
            date(),
            &day_report(Vec::new()),
            &day_summary(None),
            &[],
            "What happened?",
        );

        assert!(
            prompt.user.contains("Nothing was recorded on this day"),
            "{}",
            prompt.user
        );
    }

    /// A private session is time in an application and nothing else, on this path as
    /// much as on the summariser's.
    #[test]
    fn a_private_session_is_time_and_nothing_else() {
        let mut hidden = episode("Signal", Some("A conversation"), 900_000);
        hidden.is_private = true;
        let prompt = chat_prompt(
            date(),
            &day_report(vec![hidden]),
            &day_summary(None),
            &[],
            "What happened?",
        );

        assert!(prompt.user.contains("Signal"), "{}", prompt.user);
        assert!(!prompt.user.contains("A conversation"), "{}", prompt.user);
        assert!(prompt.user.contains("private session"), "{}", prompt.user);
    }

    /// One episode of an alternation, with an id of its own so the thread can find it
    /// again. The shared `episode` helper names every episode after its application,
    /// which is fine where an application appears once and useless here.
    fn beat(nth: usize, app: &str, title: &str, seconds: i64) -> Episode {
        Episode {
            id: format!("2026-08-22#{nth}"),
            title: Some(title.to_owned()),
            start: at(9, 5 + (nth as u32 % 50)),
            end: at(9, 6 + (nth as u32 % 50)),
            ..episode(app, Some(title), seconds * 1_000)
        }
    }

    /// The regression this whole change exists for.
    ///
    /// An essay written in Word from a draft rendered beside it produces an episode
    /// every few seconds and not one of them lasts a minute. Under the old per-episode
    /// floor the entire piece of work vanished from the prompt and the hour was
    /// described by whatever else happened to be long enough — on the evening this was
    /// found, a video. The work has to arrive as one stretch of its real size.
    #[test]
    fn work_shuttled_between_two_windows_survives_as_one_stretch() {
        let mut episodes = Vec::new();
        for nth in 0..40 {
            if nth % 2 == 0 {
                episodes.push(beat(nth, "Microsoft Word", "final crit", 20));
            } else {
                episodes.push(beat(nth, "Markdown Renderer", "06_conclusion.md", 25));
            }
        }
        let borrowed: Vec<&Episode> = episodes.iter().collect();
        let ids: Vec<&str> = borrowed.iter().map(|e| e.id.as_str()).collect();
        let prompt = hour_prompt(date(), &hour(900_000, &ids), &borrowed).unwrap();

        assert!(prompt.user.contains("final crit"), "{}", prompt.user);
        assert!(
            prompt
                .user
                .contains("Markdown Renderer with Microsoft Word"),
            "the pair is one piece of work: {}",
            prompt.user
        );
        assert!(
            prompt.user.contains("39 crossings"),
            "the crossings are counted rather than listed: {}",
            prompt.user
        );
        // Fifteen minutes of work, none of it in a visit over twenty-five seconds.
        assert!(
            prompt.user.contains("(15m)"),
            "the stretch carries its real size: {}",
            prompt.user
        );
    }

    /// The measured block is what a question about distraction is answered from, so it
    /// has to state the counts rather than leave them to be derived.
    #[test]
    fn the_prompt_states_the_switching_it_measured() {
        let mut episodes = Vec::new();
        for nth in 0..40 {
            if nth % 2 == 0 {
                episodes.push(beat(nth, "Microsoft Word", "final crit", 20));
            } else {
                episodes.push(beat(nth, "Markdown Renderer", "06_conclusion.md", 25));
            }
        }
        let report = day_report(episodes);
        let prompt = chat_prompt(
            date(),
            &report,
            &day_summary(None),
            &[],
            "How distracted was I?",
        );

        assert!(
            prompt
                .user
                .contains("How attention moved in this day, measured"),
            "{}",
            prompt.user
        );
        assert!(
            prompt.user.contains("40 foreground visits"),
            "{}",
            prompt.user
        );
        assert!(
            prompt
                .user
                .contains("A short visit is not the same as a short piece of work"),
            "the bands must not read as a verdict: {}",
            prompt.user
        );
        assert!(
            prompt
                .user
                .contains("Markdown Renderer with Microsoft Word 39 times"),
            "{}",
            prompt.user
        );
    }

    /// The instruction that stops a high count being reported as distraction on its own.
    #[test]
    fn the_chat_system_prompt_separates_coupled_switching_from_interruption() {
        assert!(CHAT_SYSTEM.contains("Switching is not distraction by itself"));
        assert!(CHAT_SYSTEM.contains("never estimate a number of your own"));
        assert!(SYSTEM.contains("crossing between such a pair"));
    }

    /// A plain question, with no steer appended, must still get a direct answer up
    /// front. This is the whole point of the instruction: a user asking "how
    /// distracted was I" should not have to phrase the question a particular way to
    /// get a verdict rather than a recitation.
    #[test]
    fn the_chat_system_prompt_asks_for_a_direct_answer_before_the_evidence() {
        assert!(CHAT_SYSTEM.contains("Open with a direct answer to the question"));
        assert!(CHAT_SYSTEM.contains("before any of the evidence"));
        assert!(CHAT_SYSTEM.contains("not a judgement of the person"));
        assert!(CHAT_SYSTEM.contains("say plainly how distracted the person was"));
    }
}
