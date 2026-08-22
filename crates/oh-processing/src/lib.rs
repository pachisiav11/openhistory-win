//! Turning recorded events into something readable.
//!
//! Nothing here talks to Windows or to Tauri. It reads the event log, groups it into
//! episodes, measures where the time went, and builds a search index — all as plain
//! functions over data, which is what makes the whole layer testable from fixtures.

pub mod day;
pub mod episode;
pub mod index;
pub mod redact;
pub mod rollup;

pub use day::{DayReport, Processor};
pub use episode::{ACTIVE_GAP, Episode, IDLE_SPLIT, detect_episodes};
pub use index::{IndexedEpisode, SearchHit, SearchIndex};
pub use redact::{PublicEpisode, public_episodes, strip_query};
pub use rollup::{AppUsage, DailyRollup, HourlyRollup, human_duration, roll_up};
