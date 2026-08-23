//! The Windows activity collector.
//!
//! Watches the foreground window, its title, and the URL of a browser tab, and
//! reports each observation as an [`oh_core::ActivityEvent`]. Screen power and
//! session lock transitions are reported too.
//!
//! The macOS build exposed seven functions to JavaScript through N-API. This port
//! has no JavaScript, but keeps the same seven operations with the same names and
//! meanings, so behaviour stays comparable across the two. See `docs/ARCHITECTURE.md`,
//! AD-1.
//!
//! ```no_run
//! use oh_collector::{CollectorConfig, start_collector};
//!
//! let collector = start_collector(
//!     Box::new(|event| println!("{}", serde_json::to_string(&event).unwrap())),
//!     CollectorConfig::default(),
//! )?;
//! // ... time passes ...
//! collector.stop();
//! # Ok::<(), anyhow::Error>(())
//! ```

pub mod browser;
pub mod collector;
pub mod config;
pub mod text;
pub mod uia;
pub mod win;

pub use collector::{Collector, EventSink};
pub use config::CollectorConfig;

use anyhow::Result;

/// Begin collecting. Equivalent to the original `startCollector`.
pub fn start_collector(sink: EventSink, config: CollectorConfig) -> Result<Collector> {
    Collector::start(sink, config)
}

/// Stop collecting. Equivalent to the original `stopCollector`.
pub fn stop_collector(collector: Collector) {
    collector.stop();
}

/// Whether the accessibility layer will answer us.
///
/// macOS gates this behind a permission the user grants in System Settings. Windows
/// has no equivalent prompt for UIAutomation, so this reports whether the automation
/// client can actually be created — which is what callers really want to know.
pub fn is_trusted() -> bool {
    let _apartment = uia::ComApartment::enter();
    uia::Automation::new().is_ok()
}

/// Ask for accessibility permission.
///
/// Nothing to request on Windows: UIAutomation needs no grant. Kept so the surface
/// matches the original, and so a caller that polls `is_trusted` after calling this
/// behaves sensibly on both platforms.
pub fn request_trust() {
    tracing::debug!("request_trust is a no-op on Windows; UIAutomation requires no grant");
}

/// This process's identifier. Equivalent to the original `processIdentifier`.
pub fn process_identifier() -> u32 {
    win::current_process_id()
}

/// Whether the foreground application can be read right now.
pub fn can_read_focused_application() -> bool {
    let Some(hwnd) = win::foreground_window() else {
        return false;
    };
    win::window_process_id(hwnd).is_some_and(|pid| win::process_image_path(pid).is_some())
}

/// Identifier for this process.
///
/// macOS returns a bundle identifier. Windows has no such concept, so this returns
/// the executable's file stem, which is the closest stable equivalent.
pub fn bundle_identifier() -> String {
    win::process_image_path(win::current_process_id())
        .map(|path| win::file_stem(&path))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seven_operations_answer() {
        assert!(process_identifier() != 0);
        assert!(!bundle_identifier().is_empty());
        assert!(
            is_trusted(),
            "UIAutomation should be available on any supported Windows"
        );
        assert!(can_read_focused_application() || win::foreground_window().is_none());
        request_trust();
    }
}
