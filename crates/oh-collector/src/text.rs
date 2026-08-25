//! What may be written down from the text a window is showing.
//!
//! Reading the accessibility tree returns whatever the application chose to publish:
//! tab labels and headings, but also the contents of a field somebody was typing a
//! card number into, and occasionally an API key sitting in a terminal. The collector
//! is a foreground recorder, not a screen reader, so this module decides what of that
//! is small enough and dull enough to keep.
//!
//! Three rules, in order: a line that looks like a secret is dropped whole, a run of
//! digits long enough to be an account number is masked, and what survives is cut to
//! a budget. The budget is the important one — an unbounded read would put a
//! document's whole body in the event log, which is a copy of the document rather
//! than a record of having worked on it.
//!
//! The budget is also contended, and who wins the contention is the fourth rule. A
//! window offers far more furniture than content — a Word window publishes several
//! hundred ribbon controls and one document — so lines arrive labelled with where they
//! came from and are served in that order. Filling a twelve-line budget in tree order
//! is what produced `["Minimize", "Restore", "Close", "Menu", …]` from a window full of
//! writing, and serving mere content first was still not enough: a chat window's
//! sidebar of past conversations stands between the frame and the conversation, and it
//! is content by any test that does not ask what the element actually is.
//!
//! Everything here is a pure function on strings, so the rules are testable without a
//! desktop, which is the reason they live apart from the UIAutomation calls.

/// The most characters one line may carry.
pub const MAX_LINE_CHARS: usize = 120;

/// Where a line came from, in the order it deserves the budget.
///
/// The distinction is made by the walk that collects them, from the control type and
/// from whether the element sits inside a toolbar or menu. It is kept here because it
/// only matters when the budget runs out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Surface {
    /// Prose: the body of a document, a message in a conversation, the contents of a
    /// text box. What somebody actually read or wrote.
    Writing,
    /// Named things around the writing: tabs, list entries, headings, panes.
    Content,
    /// The frame around all of it: buttons, menus, ribbons, window controls.
    Furniture,
}

/// How much of what a window is showing may be written down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextBudget {
    pub lines: usize,
    pub total_chars: usize,
}

impl TextBudget {
    /// Enough to recognise a window by. What every application gets.
    pub const GLANCE: TextBudget = TextBudget {
        lines: 12,
        total_chars: 1_000,
    };

    /// Enough to say what was in the window. What the applications named in
    /// `recording.deepReadApps` get.
    ///
    /// Wider, not unbounded: a page of a document is roughly two thousand characters,
    /// so this is still an excerpt of one screen rather than a copy of a file.
    pub const STUDY: TextBudget = TextBudget {
        lines: 28,
        total_chars: 2_400,
    };
}

/// Shortest line worth keeping. Single characters are toolbar glyphs and separators.
const MIN_LINE_CHARS: usize = 2;

/// A run of digits at least this long is masked.
///
/// Twelve is above any year, page number, version or time, and below the shortest
/// card and account numbers this is meant to catch.
const DIGIT_RUN: usize = 12;

/// A word at least this long, mixing letters and digits, is treated as a secret.
///
/// Real interface text does not contain twenty-character alphanumeric words. Tokens,
/// hashes and identifiers do.
const SECRET_WORD_CHARS: usize = 20;

/// Prefixes that announce a credential whatever the rest of it looks like.
const SECRET_PREFIXES: &[&str] = &[
    "sk-",
    "sk_",
    "pk_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "xox",
    "aiza",
    "bearer ",
    "basic ",
    "-----begin",
];

/// Reduce raw accessible names to the lines that may be recorded.
///
/// Writing is taken before content and content before furniture, so a budget that runs
/// out runs out on the buttons. Within each of the three, tree order is kept.
///
/// Duplicates are dropped: an interface repeats the same label in a menu, a toolbar
/// and a tooltip, and recording it three times spends the budget on nothing. The
/// window title is dropped for the same reason — it is already recorded, as the title.
pub fn redact_lines(
    raw: impl IntoIterator<Item = (Surface, String)>,
    budget: TextBudget,
    window_title: Option<&str>,
) -> Vec<String> {
    let raw: Vec<(Surface, String)> = raw.into_iter().collect();
    let title = window_title.map(collapse_whitespace);

    let mut kept: Vec<String> = Vec::new();
    let mut total = 0usize;

    for wanted in [Surface::Writing, Surface::Content, Surface::Furniture] {
        for line in raw
            .iter()
            .filter(|(surface, _)| *surface == wanted)
            .map(|(_, line)| line)
        {
            if kept.len() == budget.lines {
                break;
            }
            let Some(line) = redact_line(line) else {
                continue;
            };
            if title.as_deref().is_some_and(|title| title == line) {
                continue;
            }
            if kept.iter().any(|existing| existing == &line) {
                continue;
            }
            let length = line.chars().count();
            if total + length > budget.total_chars {
                continue;
            }
            total += length;
            kept.push(line);
        }
    }
    kept
}

/// One line reduced to what may be recorded, or `None` if none of it may be.
pub fn redact_line(raw: &str) -> Option<String> {
    let collapsed = collapse_whitespace(raw);
    if collapsed.chars().count() < MIN_LINE_CHARS {
        return None;
    }
    if looks_secret(&collapsed) || names_a_location(&collapsed) {
        return None;
    }
    Some(truncate(&mask_digit_runs(&collapsed)))
}

/// True when a line is where something lives rather than anything anybody read.
///
/// A file location names the machine's layout, which is the one thing this application
/// promises never to carry out of the event log. An Electron window publishes the
/// `file://` address of its own bundle as the name of its document element, so without
/// this the executable path reaches the timeline through the screen-text field after
/// being kept out of every other one.
fn names_a_location(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    if lowered.starts_with("file:") || lowered.starts_with(r"\\") {
        return true;
    }
    let bytes = line.as_bytes();
    bytes.len() > 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

/// Accessible names arrive with newlines, tabs and runs of spaces in them. One line
/// per element is what the schema stores, so the shape is normalized before anything
/// else looks at it.
fn collapse_whitespace(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// True when a line should not be written down at all.
fn looks_secret(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    if SECRET_PREFIXES
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
    {
        return true;
    }

    line.split_whitespace().any(|word| {
        word.chars().count() >= SECRET_WORD_CHARS
            && word.chars().any(|c| c.is_ascii_digit())
            && word.chars().any(|c| c.is_alphabetic())
            && word
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'))
    })
}

/// Replace long digit runs with an ellipsis, keeping the rest of the line.
///
/// Separators inside a number are counted as part of the run, so `4111 1111 1111
/// 1111` is caught as readily as the same digits unspaced. A run is only masked once
/// it ends, so the check cannot be fooled by a trailing letter.
fn mask_digit_runs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut run = String::new();
    let mut digits = 0usize;

    for c in line.chars() {
        if c.is_ascii_digit() {
            run.push(c);
            digits += 1;
        } else if digits > 0 && matches!(c, ' ' | '-' | '.') {
            // Possibly a separator inside a number; hold it with the run.
            run.push(c);
        } else {
            flush(&mut out, &mut run, &mut digits);
            out.push(c);
        }
    }
    flush(&mut out, &mut run, &mut digits);
    out
}

fn flush(out: &mut String, run: &mut String, digits: &mut usize) {
    if *digits >= DIGIT_RUN {
        // Trailing separators belong to whatever follows, not to the number.
        let tail: String = run
            .chars()
            .rev()
            .take_while(|c| !c.is_ascii_digit())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        out.push('…');
        out.push_str(&tail);
    } else {
        out.push_str(run);
    }
    run.clear();
    *digits = 0;
}

/// Cut a line to the budget, on a character boundary.
fn truncate(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(MAX_LINE_CHARS - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(raw: &[&str]) -> Vec<String> {
        redact_lines(
            raw.iter().map(|s| (Surface::Content, (*s).to_owned())),
            TextBudget::GLANCE,
            None,
        )
    }

    #[test]
    fn ordinary_interface_text_survives_unchanged() {
        assert_eq!(
            lines(&["Preview", "budget-2026.xlsx", "Heading 2"]),
            vec!["Preview", "budget-2026.xlsx", "Heading 2"]
        );
    }

    #[test]
    fn whitespace_is_collapsed_to_one_line() {
        assert_eq!(
            redact_line("  Chapter\n\tOne  of  Three  ").as_deref(),
            Some("Chapter One of Three")
        );
    }

    #[test]
    fn glyphs_and_separators_are_not_worth_recording() {
        assert_eq!(redact_line("x"), None);
        assert_eq!(redact_line(" "), None);
        assert_eq!(redact_line(""), None);
    }

    #[test]
    fn a_line_that_looks_like_a_credential_is_dropped_whole() {
        assert_eq!(redact_line("sk-ant-api03-abcdefghijklmnop"), None);
        assert_eq!(
            redact_line("ghp_16C7e42F292c6912E7710c838347Ae178B4a"),
            None
        );
        assert_eq!(redact_line("Bearer eyJhbGciOiJIUzI1NiJ9"), None);
        assert_eq!(redact_line("AIzaSyD-abcdefghijklmnopqrstuv"), None);
        assert_eq!(redact_line("-----BEGIN RSA PRIVATE KEY-----"), None);
        // A long mixed word anywhere in the line condemns the line.
        assert_eq!(redact_line("token: a1b2c3d4e5f6g7h8i9j0k1l2"), None);
    }

    #[test]
    fn a_long_run_of_letters_and_digits_is_treated_as_a_secret_even_in_prose() {
        // Deliberate: the cost of dropping an unusual heading is a line missing from
        // a summary, and the cost of keeping a token is a token in the event log.
        assert_eq!(redact_line("build a1b2c3d4e5f6g7h8i9j0"), None);
    }

    #[test]
    fn ordinary_long_words_are_not_mistaken_for_secrets() {
        // Long, but no digits in it.
        assert_eq!(
            redact_line("internationalization").as_deref(),
            Some("internationalization")
        );
        assert_eq!(
            redact_line("Deserialization of ActivityEvent").as_deref(),
            Some("Deserialization of ActivityEvent")
        );
    }

    #[test]
    fn long_digit_runs_are_masked_and_short_ones_are_left_alone() {
        assert_eq!(
            redact_line("Card 4111111111111111 saved").as_deref(),
            Some("Card … saved")
        );
        assert_eq!(
            redact_line("Card 4111 1111 1111 1111 saved").as_deref(),
            Some("Card … saved")
        );
        // Years, versions, times and page numbers must survive.
        assert_eq!(
            redact_line("Q3 2026 forecast, page 14, v2.11.5").as_deref(),
            Some("Q3 2026 forecast, page 14, v2.11.5")
        );
    }

    #[test]
    fn a_long_line_is_cut_to_the_budget() {
        let long = "a".repeat(400);
        let cut = redact_line(&long).unwrap();
        assert_eq!(cut.chars().count(), MAX_LINE_CHARS);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn the_same_label_is_only_recorded_once() {
        assert_eq!(lines(&["Save", "Save", "Save As"]), vec!["Save", "Save As"]);
    }

    #[test]
    fn no_more_lines_than_the_budget_allows() {
        let many: Vec<String> = (0..100).map(|n| format!("Line number {n}")).collect();
        let kept: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(lines(&kept).len(), TextBudget::GLANCE.lines);
    }

    #[test]
    fn the_whole_observation_is_capped_however_the_lines_divide() {
        // Twelve lines would fit by count; by length they must not.
        let heavy: Vec<String> = (0..TextBudget::GLANCE.lines)
            .map(|n| format!("Section {n}: {}", "long heading ".repeat(8)))
            .collect();
        let kept = lines(&heavy.iter().map(String::as_str).collect::<Vec<_>>());

        let total: usize = kept.iter().map(|line| line.chars().count()).sum();
        assert!(
            total <= TextBudget::GLANCE.total_chars,
            "{total} characters is over budget"
        );
        assert!(!kept.is_empty(), "the budget must not reject everything");
    }

    #[test]
    fn a_document_body_cannot_be_copied_through_this() {
        // One enormous element, as an editor's text area reports itself.
        let body = "The quick brown fox. ".repeat(5_000);
        let kept = lines(&[&body]);

        let total: usize = kept.iter().map(|line| line.chars().count()).sum();
        assert!(
            total <= MAX_LINE_CHARS,
            "{total} characters reached the log"
        );
    }

    /// The failure this exists for: a Word window publishes several hundred ribbon
    /// controls and one document, and reading them in tree order spent the whole
    /// budget before reaching a word of the writing.
    #[test]
    fn content_is_taken_before_the_furniture_around_it() {
        let mut raw: Vec<(Surface, String)> = (0..30)
            .map(|n| (Surface::Furniture, format!("Button {n}")))
            .collect();
        raw.push((Surface::Content, "Chapter Four: the argument".into()));

        let kept = redact_lines(raw, TextBudget::GLANCE, None);
        assert_eq!(kept[0], "Chapter Four: the argument");
        assert_eq!(kept.len(), TextBudget::GLANCE.lines);
    }

    #[test]
    fn the_window_title_is_not_recorded_a_second_time() {
        let kept = redact_lines(
            [
                (Surface::Content, "final crit - Word".to_owned()),
                (Surface::Content, "final crit".to_owned()),
            ],
            TextBudget::GLANCE,
            Some("final crit - Word"),
        );
        assert_eq!(kept, vec!["final crit"]);
    }

    /// An Electron window publishes the `file://` address of its own bundle as the
    /// name of its document element, which put the executable path into the timeline
    /// through the one field that had no guard against it.
    #[test]
    fn a_line_that_is_a_location_rather_than_something_read_is_dropped() {
        assert_eq!(
            redact_line("file:///C:/Program%20Files/WindowsApps/Something/app.asar/main.js"),
            None
        );
        assert_eq!(redact_line(r"C:\Users\someone\Documents\draft.docx"), None);
        assert_eq!(redact_line(r"\\server\share\report.docx"), None);
        // An ordinary sentence that merely contains a colon is not a location.
        assert_eq!(
            redact_line("Statement of Intent: this essay explores a theme").as_deref(),
            Some("Statement of Intent: this essay explores a theme")
        );
    }

    #[test]
    fn the_wider_budget_keeps_more_without_becoming_unbounded() {
        let many: Vec<(Surface, String)> = (0..200)
            .map(|n| (Surface::Content, format!("Paragraph {n} of the draft")))
            .collect();
        let kept = redact_lines(many, TextBudget::STUDY, None);

        assert_eq!(kept.len(), TextBudget::STUDY.lines);
        let total: usize = kept.iter().map(|line| line.chars().count()).sum();
        assert!(total <= TextBudget::STUDY.total_chars);
    }
}
