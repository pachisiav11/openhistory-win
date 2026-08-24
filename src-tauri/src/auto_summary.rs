//! Writing yesterday's day summary without being asked.
//!
//! A day's episodes stop changing at midnight, but nothing wrote its summary until
//! someone opened the day and asked for one. This checks on a timer whether
//! yesterday still needs its summary and writes it with whichever provider Settings
//! already has chosen — the same call the day view makes, just made on the user's
//! behalf instead of waiting for them to ask. See AD-30.

use std::time::Duration;

use chrono::{Local, Timelike};
use tauri::{AppHandle, Manager};

use crate::AppState;
use crate::summaries::SummaryState;

/// How often the clock is checked.
///
/// [`InferenceService::summarize_day`] does nothing but read the stored summary when
/// yesterday already has one and no hour has gone stale, so checking often costs
/// almost nothing. A check missed because the application was not running costs
/// nothing either — the next launch checks again.
const CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// The local hour after which yesterday is treated as settled enough to summarize.
///
/// Before this hour, whoever is still up is probably still the previous day as far
/// as they are concerned, even though the calendar has turned over. Writing the
/// summary out from underneath them is more likely to catch a day still being lived
/// than one that is actually finished.
const EARLIEST_LOCAL_HOUR: u32 = 5;

/// Start the background check. Runs for the life of the application; there is
/// nothing to join, the same as [`crate::mcp::start_if_enabled`]'s spawn.
pub fn spawn(handle: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            run_once(&handle).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

async fn run_once(handle: &AppHandle) {
    if Local::now().hour() < EARLIEST_LOCAL_HOUR {
        return;
    }

    let Some(app) = handle.try_state::<AppState>() else {
        return;
    };
    let Some(summaries) = handle.try_state::<SummaryState>() else {
        return;
    };

    let config = app.config();
    if !config.inference.auto_summarize {
        return;
    }

    let yesterday = oh_core::today() - chrono::Duration::days(1);
    let report = match app.processor.lock().day(yesterday) {
        Ok(report) => report,
        Err(error) => {
            tracing::warn!(%error, %yesterday, "could not read yesterday to auto-summarize it");
            return;
        }
    };

    match summaries
        .service()
        .summarize_day(&config, &report, false)
        .await
    {
        Ok(run) if run.wrote_anything() => tracing::info!(
            %yesterday,
            hours = run.hours_written.len(),
            daily = run.daily_written,
            "wrote yesterday's summary automatically"
        ),
        Ok(_) => {}
        Err(error) => tracing::debug!(%error, %yesterday, "yesterday was not auto-summarized"),
    }
}
