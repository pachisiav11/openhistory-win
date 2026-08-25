//! Runs the collector's own window read against a live window, and reports what it
//! found and what it cost.
//!
//! ```text
//! cargo run -p oh-collector --example read_probe -- "final crit"
//! cargo run -p oh-collector --example read_probe -- claude study
//! ```
//!
//! The first argument matches a window title, case-insensitively, as a substring; the
//! second chooses the budget (`glance`, the default, or `study`). Unlike `uia_dump`
//! this calls [`Automation::read_window`], so what it prints is exactly what would
//! reach the event log — which is the only way to tell whether a budget is large enough
//! without waiting for a day of history to be written.

use std::time::Instant;

use oh_collector::uia::{Automation, ComApartment, ReadBudget};
use oh_collector::win;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, EnumWindows, IsWindowVisible};
use windows::core::BOOL;

/// What the enumeration is looking for and what it found.
struct Search {
    needle: String,
    found: Vec<isize>,
}

fn main() -> anyhow::Result<()> {
    let needle = std::env::args().nth(1).unwrap_or_default();
    let deep = std::env::args()
        .nth(2)
        .is_some_and(|arg| arg.eq_ignore_ascii_case("study"));

    let mut search = Search {
        needle: needle.to_ascii_lowercase(),
        found: Vec::new(),
    };
    // SAFETY: the callback runs to completion inside this call, so the pointer to
    // `search` is live for as long as it is used.
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&raw mut search as isize)) };
    let windows = search.found;

    if windows.is_empty() {
        eprintln!("no visible window whose title contains {needle:?}");
        return Ok(());
    }

    let _apartment = ComApartment::enter();
    let automation = Automation::new()?;
    let budget = if deep {
        ReadBudget::STUDY
    } else {
        ReadBudget::GLANCE
    };

    for handle in windows {
        let hwnd = HWND(handle as *mut core::ffi::c_void);
        let title = win::window_title(hwnd);
        let started = Instant::now();
        let reading = automation.read_window(hwnd, true, true, budget, title.as_deref());
        let elapsed = started.elapsed();

        println!(
            "== {} == ({} budget, {} ms, {} of {} elements)",
            title.unwrap_or_default(),
            if deep { "study" } else { "glance" },
            elapsed.as_millis(),
            reading.visited,
            budget.elements,
        );
        if let Some((path, name)) = &reading.document {
            println!("  document: {name:?} at {path:?}");
        }
        for line in &reading.lines {
            println!("  | {line}");
        }
        println!("  ({} lines)", reading.lines.len());

        // A thin read is usually a window whose content lives in a child window the
        // accessibility tree does not bridge, so say what the children are.
        let children = child_windows(hwnd);
        if !children.is_empty() {
            println!("  child windows: {}", children.join(", "));
        }
    }
    Ok(())
}

/// Classes of a window's child windows, one level down.
fn child_windows(parent: HWND) -> Vec<String> {
    let mut classes: Vec<String> = Vec::new();
    // SAFETY: the callback runs to completion inside this call.
    let _ = unsafe {
        EnumChildWindows(
            Some(parent),
            Some(name_child),
            LPARAM(&raw mut classes as isize),
        )
    };
    classes
}

unsafe extern "system" fn name_child(hwnd: HWND, param: LPARAM) -> BOOL {
    // SAFETY: `param` is the pointer `child_windows` passed to EnumChildWindows.
    let classes = unsafe { &mut *(param.0 as *mut Vec<String>) };
    if let Some(class) = win::window_class(hwnd) {
        classes.push(class);
    }
    BOOL(1)
}

unsafe extern "system" fn collect(hwnd: HWND, param: LPARAM) -> BOOL {
    // SAFETY: `param` is the pointer main passed to EnumWindows, and main is blocked
    // inside that call for the whole of this callback's life.
    let search = unsafe { &mut *(param.0 as *mut Search) };

    if unsafe { IsWindowVisible(hwnd) }.as_bool()
        && let Some(title) = win::window_title(hwnd)
        && title.to_ascii_lowercase().contains(&search.needle)
    {
        search.found.push(hwnd.0 as isize);
    }
    BOOL(1)
}
