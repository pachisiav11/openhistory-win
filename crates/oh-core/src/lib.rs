//! Shared foundations for OpenHistory: the recorded event schema, the user's
//! settings, and the on-disk layout every other crate reads and writes.
//!
//! This crate depends on nothing else in the workspace, which is what lets the
//! collector, the processing layer, the inference layer and the MCP server all be
//! tested in isolation.

pub mod config;
pub mod event;
pub mod paths;
pub mod store;

pub use config::{Config, DEFAULT_EXCLUDED, RecordingConfig};
pub use event::{
    ActivityEvent, ApplicationDescriptor, BrowserObservation, DocumentObservation, EventKind,
    SCHEMA_VERSION, SemanticElement, TextChange,
};
pub use store::{DayStats, EventStore, local_date_of, read_day, today};
