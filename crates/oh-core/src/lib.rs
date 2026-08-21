//! Shared foundations for OpenHistory: the recorded event schema and the on-disk
//! layout every other crate reads and writes.
//!
//! This crate depends on nothing else in the workspace, which is what lets the
//! collector, the processing layer, the inference layer and the MCP server all be
//! tested in isolation.

pub mod event;
pub mod paths;

pub use event::{
    ActivityEvent, ApplicationDescriptor, BrowserObservation, DocumentObservation, EventKind,
    SCHEMA_VERSION, SemanticElement, TextChange,
};
