//! What the collector refuses to record.
//!
//! The settings themselves live in `oh_core::config`, because they are persisted with
//! the rest of the user's configuration and the application must be able to read and
//! write them without depending on the collector. This module is the collector's view
//! of them.

pub use oh_core::config::{DEFAULT_EXCLUDED, RecordingConfig as CollectorConfig};
