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
//! Everything here is a pure function on strings, so the rules are testable without a
//! desktop, which is the reason they live apart from the UIAutomation calls.

/// The most lines one observation may carry.
pub const MAX_LINES: usize = 12;

/// The most characters one line may carry.
pub const MAX_LINE_CHARS: usize = 120;

/// The most characters one observation may carry across every line.
pub const MAX_TOTAL_CHARS: usize = 1_000;

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
/// Duplicates are dropped: an interface repeats the same label in a menu, a toolbar
/// and a tooltip, and recording it three times spends the budget on nothing.
pub fn redact_lines(raw: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    let mut total = 0usize;

    for line in raw {
        if kept.len() == MAX_LINES {
            break;
        }
        let Some(line) = redact_line(&line) else {
            continue;
        };
        if kept.iter().any(|existing| existing == &line) {
            continue;
        }
        let length = line.chars().count();
        if total + length > MAX_TOTAL_CHARS {
            continue;
        }
        total += length;
        kept.push(line);
    }
    kept
}

/// One line reduced to what may be recorded, or `None` if none of it may be.
pub fn redact_line(raw: &str) -> Option<String> {
    let collapsed = collapse_whitespace(raw);
    if collapsed.chars().count() < MIN_LINE_CHARS {
        return None;
    }
    if looks_secret(&collapsed) {
        return None;
    }
    Some(truncate(&mask_digit_runs(&collapsed)))
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
        redact_lines(raw.iter().map(|s| (*s).to_owned()))
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
        assert_eq!(redact_lines(many).len(), MAX_LINES);
    }

    #[test]
    fn the_whole_observation_is_capped_however_the_lines_divide() {
        // Twelve lines would fit by count; by length they must not.
        let heavy: Vec<String> = (0..MAX_LINES)
            .map(|n| format!("Section {n}: {}", "long heading ".repeat(8)))
            .collect();
        let kept = redact_lines(heavy);

        let total: usize = kept.iter().map(|line| line.chars().count()).sum();
        assert!(
            total <= MAX_TOTAL_CHARS,
            "{total} characters is over budget"
        );
        assert!(!kept.is_empty(), "the budget must not reject everything");
    }

    #[test]
    fn a_document_body_cannot_be_copied_through_this() {
        // One enormous element, as an editor's text area reports itself.
        let body = "The quick brown fox. ".repeat(5_000);
        let kept = redact_lines([body]);

        let total: usize = kept.iter().map(|line| line.chars().count()).sum();
        assert!(
            total <= MAX_LINE_CHARS,
            "{total} characters reached the log"
        );
    }
}
