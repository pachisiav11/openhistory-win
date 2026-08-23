//! Keeping a day's summary, and the documents kept so far.
//!
//! The summaries under `summaries/` are derived and disposable: forgetting a day or
//! clearing the history takes them, and a retention window eventually takes the events
//! they were written from, after which no amount of reprocessing brings them back.
//! Saving one to the library is how a day stops being disposable.
//!
//! What is saved is a Markdown document rather than the summary alone, because the
//! summary is only meaningful beside the measurements it describes. Composition lives
//! here rather than in `oh-core` because it needs the day's report as well as its
//! summary, and the store deliberately knows about neither.

use chrono::NaiveDate;
use oh_core::{DaySummary, LibraryEntry, LibraryStore};
use oh_processing::rollup::human_duration;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::{AppState, parse_date, to_message};

/// Every document in the library, newest first.
#[tauri::command]
pub fn library_entries() -> Result<Vec<LibraryEntry>, String> {
    Ok(LibraryStore::open().map_err(to_message)?.list())
}

/// One document's Markdown, without the front matter the interface has no use for.
#[tauri::command]
pub fn library_document(id: String) -> Result<String, String> {
    LibraryStore::open()
        .map_err(to_message)?
        .body(&id)
        .map_err(to_message)
}

/// Compose a day into a document and keep it.
#[tauri::command]
pub fn library_save(
    date: String,
    app: State<'_, AppState>,
    summaries: State<'_, crate::summaries::SummaryState>,
) -> Result<LibraryEntry, String> {
    let day: NaiveDate = parse_date(&date)?;
    let report = app.processor.lock().day(day).map_err(to_message)?;
    let summary = summaries.service().summary(day);

    let title = day_title(day);
    let body = compose(day, &report, &summary);

    LibraryStore::open()
        .map_err(to_message)?
        .save(&date, &title, &body)
        .map_err(to_message)
}

#[tauri::command]
pub fn library_delete(id: String) -> Result<(), String> {
    LibraryStore::open()
        .map_err(to_message)?
        .delete(&id)
        .map_err(to_message)
}

/// Write a copy of a document wherever the user chooses.
///
/// Returns the path it went to, or `None` when the dialog was dismissed. Dismissing a
/// save dialog is an ordinary thing to do and is not an error to report.
#[tauri::command]
pub fn library_export(app: AppHandle, id: String) -> Result<Option<String>, String> {
    let store = LibraryStore::open().map_err(to_message)?;
    // Read before asking. A document that cannot be read should say so rather than
    // open a dialog and fail after the user has chosen where to put it.
    store.read(&id).map_err(to_message)?;

    let chosen = app
        .dialog()
        .file()
        .set_title("Export summary")
        .set_file_name(format!("{id}.md"))
        .add_filter("Markdown", &["md"])
        .blocking_save_file();

    let Some(destination) = chosen else {
        return Ok(None);
    };
    let destination = destination
        .into_path()
        .map_err(|error| format!("that destination cannot be written to: {error}"))?;

    oh_core::library::export(&store, &id, &destination).map_err(to_message)?;
    Ok(Some(destination.display().to_string()))
}

/// What a day's document is called.
pub fn day_title(date: NaiveDate) -> String {
    date.format("%A %-d %B %Y").to_string()
}

/// A day as a Markdown document.
///
/// Written to stand on its own once it leaves the application: somebody reading the
/// file in a year should not need the numbers beside it to know what the day was.
pub fn compose(date: NaiveDate, report: &oh_processing::DayReport, summary: &DaySummary) -> String {
    let mut out = format!("# {}\n\n", day_title(date));
    let rollup = &report.rollup;

    match summary.daily.as_deref() {
        Some(text) => {
            out.push_str(text.trim());
            out.push_str("\n\n");
        }
        None => out.push_str("No whole-day summary was written.\n\n"),
    }

    out.push_str("## Where the time went\n\n");
    if rollup.screen_ms() == 0 {
        out.push_str("Nothing was recorded on this day.\n\n");
    } else {
        out.push_str(&format!(
            "{} at the machine, {} of it working.\n\n",
            human_duration(rollup.screen_ms()),
            human_duration(rollup.active_ms),
        ));
        for usage in &rollup.apps {
            out.push_str(&format!(
                "- {} — {}\n",
                usage.app,
                human_duration(usage.active_ms)
            ));
        }
        if rollup.idle_ms > 0 {
            out.push_str(&format!("- Idle — {}\n", human_duration(rollup.idle_ms)));
        }
        out.push('\n');
    }

    if !summary.hours.is_empty() {
        out.push_str("## Hours\n\n");
        for hour in &summary.hours {
            out.push_str(&format!(
                "### {:02}:00 — {}\n\n{}\n\n",
                hour.hour,
                human_duration(hour.active_ms),
                hour.text.trim()
            ));
        }
    }

    // Which model said it matters as much as what it said, and the file will outlive
    // whatever the settings hold by the time anyone reads it.
    if let Some(model) = summary
        .hours
        .first()
        .map(|hour| hour.model.clone())
        .or_else(|| {
            summary
                .daily
                .as_ref()
                .map(|_| "an unnamed model".to_owned())
        })
    {
        out.push_str(&format!("---\n\nSummaries written by {model}.\n"));
    }

    out
}

#[cfg(test)]
mod tests {
    use oh_core::HourSummary;
    use oh_processing::DayReport;
    use oh_processing::rollup::{AppUsage, DailyRollup};

    use super::*;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 22).unwrap()
    }

    fn report() -> DayReport {
        DayReport {
            date: "2026-08-22".into(),
            episodes: Vec::new(),
            rollup: DailyRollup {
                date: "2026-08-22".into(),
                active_ms: 45 * 60_000,
                idle_ms: 15 * 60_000,
                episodes: 4,
                apps: vec![AppUsage {
                    app: "Visual Studio Code".into(),
                    active_ms: 45 * 60_000,
                    episodes: 4,
                }],
                hours: Vec::new(),
                first_activity: None,
                last_activity: None,
                private_episodes: 0,
            },
        }
    }

    fn summary() -> DaySummary {
        let mut summary = DaySummary::new(date());
        summary.set_daily("A morning of Rust and a short read.");
        summary.set_hour(HourSummary {
            hour: 9,
            text: "Worked through the collector.".into(),
            active_ms: 45 * 60_000,
            generated_at: "2026-08-22T10:00:00.000Z".into(),
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
        });
        summary
    }

    #[test]
    fn a_document_carries_the_summary_the_measurements_and_the_model() {
        let document = compose(date(), &report(), &summary());

        assert!(document.starts_with("# Saturday 22 August 2026\n"));
        assert!(document.contains("A morning of Rust and a short read."));
        assert!(document.contains("1h at the machine, 45m of it working."));
        assert!(document.contains("- Visual Studio Code — 45m"));
        assert!(document.contains("- Idle — 15m"));
        assert!(document.contains("### 09:00 — 45m"));
        assert!(document.contains("Worked through the collector."));
        assert!(document.contains("written by claude-haiku-4-5"));
    }

    #[test]
    fn a_day_with_no_summary_is_still_worth_keeping() {
        let document = compose(date(), &report(), &DaySummary::new(date()));

        assert!(document.contains("No whole-day summary was written."));
        assert!(document.contains("- Visual Studio Code — 45m"));
        assert!(
            !document.contains("## Hours"),
            "an empty section is worse than no section"
        );
    }

    #[test]
    fn a_day_with_nothing_recorded_says_so_rather_than_showing_zeroes() {
        let mut empty = report();
        empty.rollup.active_ms = 0;
        empty.rollup.idle_ms = 0;
        empty.rollup.apps.clear();

        let document = compose(date(), &empty, &DaySummary::new(date()));
        assert!(document.contains("Nothing was recorded on this day."));
        assert!(!document.contains("0s"));
    }
}
