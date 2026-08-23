//! Shared foundations for OpenHistory: the recorded event schema, the user's
//! settings, and the on-disk layout every other crate reads and writes.
//!
//! This crate depends on nothing else in the workspace, which is what lets the
//! collector, the processing layer, the inference layer and the MCP server all be
//! tested in isolation.

pub mod config;
pub mod event;
pub mod library;
pub mod paths;
pub mod store;
pub mod summary;

pub use config::{
    CLOUD_MODELS, CloudModelChoice, Config, DEFAULT_CLOUD_MODEL, DEFAULT_EXCLUDED,
    DEFAULT_MCP_PORT, InferenceConfig, InferenceProvider, McpConfig, RecordingConfig, cloud_model,
    provider_for_model,
};
pub use event::{
    ActivityEvent, ApplicationDescriptor, BrowserObservation, DocumentObservation, EventKind,
    SCHEMA_VERSION, SemanticElement, TextChange,
};
pub use library::{LibraryEntry, LibraryStore};
pub use store::{DayStats, EventStore, local_date_of, read_day, today};
pub use summary::{DaySummary, HourSummary, SummaryStore};
