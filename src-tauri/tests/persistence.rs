//! Phase 2 and Phase 3 test gates: the collector reaches disk, and what reaches disk
//! becomes episodes.
//!
//! The tests that need a desktop are `#[ignore]`d. They relocate the whole data tree
//! with `OPENHISTORY_DATA_DIR`, which is a process-wide environment variable, so they
//! must not run concurrently:
//!
//! ```text
//! cargo test -p openhistory-win --test persistence -- --ignored --test-threads=1
//! ```

use std::time::{Duration, Instant};

use oh_core::{ActivityEvent, Config, EventKind, paths};
use oh_processing::Processor;
use openhistory_win_lib::collector_service::CollectorService;

const DEADLINE: Duration = Duration::from_secs(12);

/// Point the data tree at a fresh directory for the duration of one test.
fn isolated_data_dir() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("temp data dir");
    // SAFETY: these tests are documented as single-threaded; no other thread in this
    // binary reads the environment while it is being set.
    unsafe { std::env::set_var(paths::DATA_DIR_ENV, temp.path()) };
    temp
}

fn today_on_disk() -> Vec<ActivityEvent> {
    oh_core::read_day(oh_core::today()).expect("today's log must be readable")
}

fn poll_until(mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        if ready() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    ready()
}

fn is_winver(event: &ActivityEvent) -> bool {
    is_exe(event, "winver.exe")
}

fn is_exe(event: &ActivityEvent, exe: &str) -> bool {
    event
        .application
        .as_ref()
        .is_some_and(|app| app.path.to_ascii_lowercase().ends_with(exe))
}

/// The Phase 2 gate. Recording a real desktop session produces a readable JSONL log
/// under the data directory, and stopping the service flushes it.
#[test]
#[ignore = "records a real desktop session"]
fn a_real_session_lands_in_the_event_log() {
    let temp = isolated_data_dir();
    let service = CollectorService::detached();
    service
        .start(&Config::default())
        .expect("the service must start");
    assert!(service.is_running());

    let mut child = std::process::Command::new("winver.exe")
        .spawn()
        .expect("winver must launch");
    let recorded = poll_until(|| today_on_disk().iter().any(is_winver));

    let _ = child.kill();
    let _ = child.wait();
    service.stop();

    assert!(
        !service.is_running(),
        "the service must report itself stopped"
    );
    assert!(
        recorded,
        "no winver event reached the log. Got:\n{:#?}",
        today_on_disk()
    );

    let events = today_on_disk();
    assert_eq!(
        events.first().map(|e| e.kind),
        Some(EventKind::CollectorStarted),
        "the log must open with collectorStarted"
    );

    // Everything the collector produced before `stop` returned is on disk: the count
    // the service reports and the count in the file agree.
    let status = service.status();
    assert_eq!(status.events_today, events.len());
    assert_eq!(
        status.last_event_at.as_deref(),
        events.last().map(|e| e.timestamp.as_str())
    );

    // The file really is JSON Lines: one complete object per line, no trailing junk.
    let path = temp
        .path()
        .join("events")
        .join(format!("{}.jsonl", oh_core::today().format("%Y-%m-%d")));
    let text = std::fs::read_to_string(&path).expect("the day's log must exist");
    assert_eq!(text.lines().count(), events.len());
    for line in text.lines() {
        serde_json::from_str::<ActivityEvent>(line).expect("every line must parse");
    }

    println!("{} recorded {} events:", path.display(), events.len());
    for event in &events {
        println!(
            "  {:?} {} {:?}",
            event.kind,
            event
                .application
                .as_ref()
                .map(|a| a.name.as_str())
                .unwrap_or("-"),
            event.window_title
        );
    }
}

/// Restarting under new settings keeps recording into the same day's log.
#[test]
#[ignore = "starts the collector twice against a real desktop"]
fn reconfiguring_keeps_recording_and_keeps_the_history() {
    let _temp = isolated_data_dir();
    let service = CollectorService::detached();

    let mut config = Config::default();
    service.start(&config).expect("the service must start");
    assert!(poll_until(|| !today_on_disk().is_empty()));
    let before = today_on_disk().len();

    config.recording.exclude("winver");
    service
        .reconfigure(&config)
        .expect("the service must reconfigure");

    assert!(
        service.is_running(),
        "reconfiguring must not leave recording switched off"
    );
    assert!(
        poll_until(|| today_on_disk().len() > before),
        "the restarted collector must append rather than start a new log"
    );

    service.stop();
    let events = today_on_disk();
    assert!(
        events
            .iter()
            .filter(|e| e.kind == EventKind::CollectorStarted)
            .count()
            >= 2,
        "each start must be marked in the log"
    );
}

/// The Phase 3 gate. A recorded session becomes episodes with measurable time in
/// them, and those episodes are searchable.
///
/// Two applications are brought to the foreground in turn, with a pause between them,
/// so the first episode has an end that is later than its start. A single-event
/// episode is legitimate but carries no measurable time, and the gate is about time.
///
/// Both are plain Win32 executables. A packaged application such as Windows 11's
/// Notepad launches through a stub, so killing what was spawned leaves the real window
/// running on the desktop.
#[test]
#[ignore = "records a real desktop session"]
fn a_recorded_session_becomes_searchable_episodes() {
    let temp = isolated_data_dir();
    let service = CollectorService::detached();
    service
        .start(&Config::default())
        .expect("the service must start");

    let mut winver = std::process::Command::new("winver.exe")
        .spawn()
        .expect("winver must launch");
    let saw_winver = poll_until(|| today_on_disk().iter().any(is_winver));
    std::thread::sleep(Duration::from_secs(3));

    let mut charmap = std::process::Command::new("charmap.exe")
        .spawn()
        .expect("charmap must launch");
    let saw_charmap = poll_until(|| today_on_disk().iter().any(|e| is_exe(e, "charmap.exe")));
    std::thread::sleep(Duration::from_secs(3));

    let _ = winver.kill();
    let _ = winver.wait();
    let _ = charmap.kill();
    let _ = charmap.wait();
    service.stop();

    assert!(saw_winver, "winver never reached the log");
    assert!(saw_charmap, "charmap never reached the log");

    let mut processor = Processor::in_root(temp.path()).expect("the processor must open");
    let report = processor
        .process_day(oh_core::today())
        .expect("today must process");

    println!("{} episodes:", report.episodes.len());
    for episode in &report.episodes {
        println!("  {}", episode.describe());
    }

    assert!(
        report.episodes.len() >= 2,
        "two applications in the foreground must make at least two episodes, got {}",
        report.episodes.len()
    );
    assert!(
        report.rollup.active_ms > 0,
        "the day must have measurable active time"
    );
    assert!(
        report.rollup.hours.iter().any(|hour| hour.active_ms > 0),
        "at least one hour must carry time"
    );
    assert_eq!(
        report.rollup.hours.iter().map(|h| h.active_ms).sum::<i64>(),
        report.rollup.active_ms,
        "the hours must account for exactly the day's total"
    );
    assert!(
        report.rollup.apps.iter().any(|app| app.active_ms > 0),
        "time must be attributed to an application"
    );

    // The episodes reached the index on the way out, so they are findable by the name
    // the desktop gave them. That name is taken from the report rather than written in
    // here: `winver.exe` calls itself "Version Reporter Applet", and the index holds
    // what was displayed, not what was launched.
    let named = report
        .episodes
        .iter()
        .find(|episode| !episode.is_private)
        .expect("at least one episode must be describable");
    let term = named
        .app
        .split_whitespace()
        .next()
        .expect("a name to search");
    let hits = processor.search(term, 10);
    assert!(
        hits.iter().any(|hit| hit.episode.id == named.id),
        "searching for {term:?} must find the episode it names"
    );

    // Everything is derived, so processing the same day twice changes nothing.
    let again = processor
        .process_day(oh_core::today())
        .expect("reprocessing must work");
    assert_eq!(report, again, "processing must be deterministic");
}

#[test]
fn a_service_that_never_started_reports_itself_stopped() {
    let _temp = isolated_data_dir();
    let service = CollectorService::detached();

    let status = service.status();
    assert!(!status.running);
    assert_eq!(status.events_today, 0);
    assert!(status.last_event_at.is_none());
    assert!(
        !status.data_dir.is_empty(),
        "the interface needs somewhere to point at"
    );
}

#[test]
fn stopping_a_stopped_service_is_harmless() {
    let _temp = isolated_data_dir();
    let service = CollectorService::detached();
    service.stop();
    service.stop();
    assert!(!service.is_running());
}
