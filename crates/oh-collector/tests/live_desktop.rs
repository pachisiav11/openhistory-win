//! Phase 1 test gate, run against the real desktop.
//!
//! These are `#[ignore]`d because they launch visible windows and depend on an
//! interactive session, which a build agent may not have. Run them deliberately:
//!
//! ```text
//! cargo test -p oh-collector --test live_desktop -- --ignored --nocapture
//! ```
//!
//! Session lock and screen sleep are exercised by posting the exact messages Windows
//! posts, rather than by locking the machine the tests are running on. See
//! `docs/ARCHITECTURE.md`, AD-7.

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use oh_collector::{CollectorConfig, start_collector};
use oh_core::{ActivityEvent, EventKind};

/// Long enough for a GUI process to create its window and take the foreground on a
/// loaded machine, short enough that a genuine failure is not a long wait.
const DEADLINE: Duration = Duration::from_secs(12);

fn wait_for(
    events: &Receiver<ActivityEvent>,
    seen: &mut Vec<ActivityEvent>,
    mut predicate: impl FnMut(&ActivityEvent) -> bool,
) -> Option<ActivityEvent> {
    if let Some(found) = seen.iter().find(|e| predicate(e)) {
        return Some(found.clone());
    }

    let deadline = Instant::now() + DEADLINE;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(remaining) {
            Ok(event) => {
                let matched = predicate(&event);
                seen.push(event.clone());
                if matched {
                    return Some(event);
                }
            }
            Err(_) => break,
        }
    }
    None
}

fn describe(seen: &[ActivityEvent]) -> String {
    seen.iter()
        .map(|e| {
            format!(
                "  {:?} {} | title {:?} | text {:?}",
                e.kind,
                e.application
                    .as_ref()
                    .map(|a| a.name.as_str())
                    .unwrap_or("-"),
                e.window_title.as_deref().unwrap_or("-"),
                e.visible_text.as_deref().unwrap_or(&[])
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
#[ignore = "launches a window and needs an interactive desktop"]
fn records_a_real_application_switch() {
    let (tx, events) = mpsc::channel();
    let collector = start_collector(
        Box::new(move |event| {
            let _ = tx.send(event);
        }),
        CollectorConfig::default(),
    )
    .expect("collector must start");

    let mut seen = Vec::new();

    wait_for(&events, &mut seen, |e| {
        e.kind == EventKind::CollectorStarted
    })
    .expect("collectorStarted must be the first thing reported");

    // winver.exe is a plain Win32 dialog that is present on every Windows install and
    // is not repackaged as a Store app, so the window really does belong to the pid
    // we spawned.
    let mut child = std::process::Command::new("winver.exe")
        .spawn()
        .expect("winver must launch");

    let activated = wait_for(&events, &mut seen, |e| {
        e.kind == EventKind::ApplicationActivated
            && e.application
                .as_ref()
                .is_some_and(|a| a.path.to_ascii_lowercase().ends_with("winver.exe"))
    });

    let activated = activated.unwrap_or_else(|| {
        let _ = child.kill();
        panic!(
            "no applicationActivated for winver. Saw:\n{}",
            describe(&seen)
        )
    });

    let application = activated.application.as_ref().unwrap();
    assert!(application.pid != 0);
    assert!(
        !application.name.is_empty(),
        "application name must be populated"
    );
    assert!(application.bundle_id.is_none(), "bundleId is a macOS field");
    assert_eq!(activated.version, 1);
    assert!(
        activated.time().is_some(),
        "timestamp must parse as RFC 3339"
    );

    child.kill().expect("winver must be killable");
    let _ = child.wait();

    // Closing winver hands the foreground back, which is when the collector notices
    // the process is gone.
    wait_for(&events, &mut seen, |e| {
        e.kind == EventKind::ApplicationTerminated
    })
    .unwrap_or_else(|| panic!("no applicationTerminated. Saw:\n{}", describe(&seen)));

    collector.stop();
}

#[test]
#[ignore = "needs an interactive desktop for the collector to start"]
fn reports_session_lock_and_unlock() {
    let (tx, events) = mpsc::channel();
    let collector = start_collector(
        Box::new(move |event| {
            let _ = tx.send(event);
        }),
        CollectorConfig::default(),
    )
    .expect("collector must start");

    let mut seen = Vec::new();
    wait_for(&events, &mut seen, |e| {
        e.kind == EventKind::CollectorStarted
    })
    .expect("collectorStarted must arrive");

    collector.inject_session_change(true);
    let locked = wait_for(&events, &mut seen, |e| e.kind == EventKind::SessionLocked)
        .unwrap_or_else(|| panic!("no sessionLocked. Saw:\n{}", describe(&seen)));
    assert!(
        locked.application.is_none(),
        "session events describe the session, not an app"
    );

    collector.inject_session_change(false);
    wait_for(&events, &mut seen, |e| e.kind == EventKind::SessionUnlocked)
        .unwrap_or_else(|| panic!("no sessionUnlocked. Saw:\n{}", describe(&seen)));

    collector.stop();
}

/// A browser installed on this machine, and the switch that opens it privately.
struct PrivateBrowser {
    exe: std::path::PathBuf,
    private_flag: &'static str,
}

fn installed_browsers() -> Vec<PrivateBrowser> {
    let roots: Vec<std::path::PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .map(std::path::PathBuf::from)
        .collect();

    let candidates = [
        (
            ["Google", "Chrome", "Application", "chrome.exe"],
            "--incognito",
        ),
        (
            ["Microsoft", "Edge", "Application", "msedge.exe"],
            "--inprivate",
        ),
    ];

    let mut found = Vec::new();
    for (segments, private_flag) in candidates {
        for root in &roots {
            let exe = segments
                .iter()
                .fold(root.clone(), |path, segment| path.join(segment));
            if exe.is_file() {
                found.push(PrivateBrowser { exe, private_flag });
                break;
            }
        }
    }
    found
}

/// The central privacy guarantee: a private browser window is acknowledged and
/// nothing about it is recorded.
///
/// This must run against real browsers. Current Chrome titles an incognito window
/// exactly like an ordinary one, so a unit test over title strings would pass while
/// the product silently recorded private sessions — which is precisely what happened
/// before this test existed. A throwaway profile directory keeps the spawned browser
/// in its own process tree, so shutting it down never touches an existing session.
#[test]
#[ignore = "opens and closes real private browser windows"]
fn private_browsing_records_nothing_but_the_boundary() {
    let browsers = installed_browsers();
    assert!(
        !browsers.is_empty(),
        "no supported browser is installed to test against"
    );

    for browser in browsers {
        let label = browser
            .exe
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let profile = tempfile::tempdir().expect("temp profile dir");

        let (tx, events) = mpsc::channel();
        let collector = start_collector(
            Box::new(move |event| {
                let _ = tx.send(event);
            }),
            CollectorConfig::default(),
        )
        .expect("collector must start");

        let mut seen = Vec::new();
        wait_for(&events, &mut seen, |e| {
            e.kind == EventKind::CollectorStarted
        })
        .unwrap();

        let mut child = std::process::Command::new(&browser.exe)
            .arg(browser.private_flag)
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .arg("about:blank")
            .spawn()
            .unwrap_or_else(|e| panic!("{label} must launch: {e}"));

        let boundary = wait_for(&events, &mut seen, |e| e.kind == EventKind::PrivacyBoundary);

        // Shut the browser down before asserting, so a failed expectation never
        // leaves a stray window on the desktop. These browsers spawn helper
        // processes, so the whole tree has to go.
        let _ = std::process::Command::new("taskkill.exe")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .output();
        let _ = child.wait();
        collector.stop();

        let boundary = boundary.unwrap_or_else(|| {
            panic!(
                "no privacyBoundary for a private {label} window. Saw:\n{}",
                describe(&seen)
            )
        });

        assert!(
            boundary.window_title.is_none(),
            "{label}: a private window's title must never be recorded"
        );
        let observation = boundary
            .browser
            .as_ref()
            .unwrap_or_else(|| panic!("{label}: privacyBoundary must carry a browser observation"));
        assert!(
            observation.is_private,
            "{label}: the observation must be marked private"
        );
        assert!(
            observation.url.is_none(),
            "{label}: a private URL must never be recorded"
        );
        assert!(
            boundary.is_private(),
            "{label}: the event must classify itself as private"
        );

        // Nothing else about that session may have leaked into the stream.
        let private_pid = boundary.application.as_ref().map(|a| a.pid);
        for event in &seen {
            if event.kind == EventKind::PrivacyBoundary {
                continue;
            }
            let same_session = event.application.as_ref().map(|a| a.pid) == private_pid;
            assert!(
                !same_session,
                "{label}: the private session leaked a {:?} event: {:?}",
                event.kind, event.window_title
            );
        }
    }
}

#[test]
#[ignore = "needs an interactive desktop for the collector to start"]
fn excluded_applications_are_never_reported() {
    let mut config = CollectorConfig::default();
    config.exclude("winver");

    let (tx, events) = mpsc::channel();
    let collector = start_collector(
        Box::new(move |event| {
            let _ = tx.send(event);
        }),
        config,
    )
    .expect("collector must start");

    let mut seen = Vec::new();
    wait_for(&events, &mut seen, |e| {
        e.kind == EventKind::CollectorStarted
    })
    .unwrap();

    let mut child = std::process::Command::new("winver.exe")
        .spawn()
        .expect("winver must launch");

    // Give the collector the same window of opportunity the positive test gets, and
    // require that it stays silent about this application.
    let leaked = wait_for(&events, &mut seen, |e| {
        e.application
            .as_ref()
            .is_some_and(|a| a.path.to_ascii_lowercase().ends_with("winver.exe"))
    });

    let _ = child.kill();
    let _ = child.wait();
    collector.stop();

    assert!(
        leaked.is_none(),
        "an excluded application reached the event stream: {leaked:?}"
    );
}

#[test]
#[ignore = "launches a window and needs an interactive desktop"]
fn reads_what_a_real_window_is_showing() {
    let (tx, events) = mpsc::channel();
    let collector = start_collector(
        Box::new(move |event| {
            let _ = tx.send(event);
        }),
        CollectorConfig::default(),
    )
    .expect("collector must start");

    let mut seen = Vec::new();
    wait_for(&events, &mut seen, |e| {
        e.kind == EventKind::CollectorStarted
    })
    .expect("collectorStarted must be the first thing reported");

    // Character Map is a plain Win32 window with a lot of labelled interface: group
    // boxes, buttons and a font list. If a real window's text can be read at all,
    // it can be read here.
    let mut child = std::process::Command::new("charmap.exe")
        .spawn()
        .expect("charmap must launch");

    let observed = wait_for(&events, &mut seen, |e| {
        e.application
            .as_ref()
            .is_some_and(|a| a.path.to_ascii_lowercase().ends_with("charmap.exe"))
            && e.visible_text.is_some()
    });

    let observed = observed.unwrap_or_else(|| {
        let _ = child.kill();
        panic!(
            "no window observation carrying visible text for charmap. Saw:\n{}",
            describe(&seen)
        )
    });

    let lines = observed.visible_text.as_ref().unwrap();
    assert!(
        !lines.is_empty(),
        "an absent read must not be an empty list"
    );
    assert!(
        lines.len() <= oh_collector::text::TextBudget::STUDY.lines,
        "the budget was exceeded: {lines:?}"
    );
    for line in lines {
        assert!(
            line.chars().count() <= oh_collector::text::MAX_LINE_CHARS,
            "a line was over budget: {line}"
        );
    }
    println!("charmap showed: {lines:?}");

    child.kill().expect("charmap must be killable");
    let _ = child.wait();
    collector.stop();
}

/// An ordinary Chromium window must yield more than its own name.
///
/// `charmap.exe` is a plain Win32 window whose whole interface is in the accessibility
/// tree before anyone asks for it. A Chromium window is not: the tree is built on
/// demand when a UIA client first reaches in, and the first read pays for waking it.
/// The installed build read `["Claude"]` and `["Notepad"]` from windows full of text
/// for exactly this reason, and no unit test could see it. A throwaway profile keeps
/// this out of the user's own browser session.
#[test]
#[ignore = "opens a real browser window and needs an interactive desktop"]
fn a_chromium_window_yields_more_than_its_own_name() {
    const PROBE_TITLE: &str = "OpenHistory Capture Probe";
    const PROBE_PAGE: &str = concat!(
        "<!doctype html><title>OpenHistory Capture Probe</title>",
        "<h1>Quarterly planning notes</h1>",
        "<p>The renewal deadline is the fourteenth.</p>",
        "<p>Ask the design team about the onboarding flow.</p>",
    );

    let Some(browser) = installed_browsers().into_iter().next() else {
        panic!("no Chromium browser is installed to test against");
    };
    let label = browser
        .exe
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    // Chromium refuses a top-level `data:` navigation, so the page has to be a file.
    let page = tempfile::tempdir().expect("temp page dir");
    let path = page.path().join("capture-probe.html");
    std::fs::write(&path, PROBE_PAGE).expect("probe page must be written");
    let profile = tempfile::tempdir().expect("temp profile dir");

    let (tx, events) = mpsc::channel();
    let collector = start_collector(
        Box::new(move |event| {
            let _ = tx.send(event);
        }),
        CollectorConfig::default(),
    )
    .expect("collector must start");

    let mut seen = Vec::new();
    wait_for(&events, &mut seen, |e| {
        e.kind == EventKind::CollectorStarted
    })
    .expect("collectorStarted must be the first thing reported");

    let mut child = std::process::Command::new(&browser.exe)
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg(format!("--user-data-dir={}", profile.path().display()))
        .arg(&path)
        .spawn()
        .unwrap_or_else(|e| panic!("{label} must launch: {e}"));

    // The user may already have this browser open on something else, so the window we
    // launched has to be told apart from the one they were using. The probe page names
    // itself in both places a window can say what it is on, and which of the two is
    // current depends on whether the page had finished loading when the read happened.
    let names_the_probe = |text: &str| text.contains(PROBE_TITLE);
    let observed = wait_for(&events, &mut seen, |e| {
        let titled = e.window_title.as_deref().is_some_and(names_the_probe);
        let shown = e
            .visible_text
            .as_ref()
            .is_some_and(|lines| lines.iter().any(|line| names_the_probe(line)));
        (titled || shown)
            && e.visible_text
                .as_ref()
                .is_some_and(|lines| !lines.is_empty())
    });

    // Shut the browser down before asserting, so a failed expectation never leaves a
    // stray window on the desktop. Chromium spawns helpers, so the whole tree has to go.
    let _ = std::process::Command::new("taskkill.exe")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .output();
    let _ = child.wait();
    collector.stop();

    let observed = observed.unwrap_or_else(|| {
        panic!(
            "no window observation carrying visible text for the probe page. Saw:\n{}",
            describe(&seen)
        )
    });
    let lines = observed.visible_text.as_ref().unwrap();
    println!("{label} showed: {lines:?}");

    assert!(
        lines.len() > 1,
        "{label} yielded only its own name: {lines:?}. The read budget is being spent \
         waking the accessibility tree rather than reading it."
    );
}
