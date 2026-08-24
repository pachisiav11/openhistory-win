//! Writing summaries of a day, with a cloud model or a local GGUF one.
//!
//! Four providers, one policy: Anthropic, OpenAI and Google AI Studio in the cloud,
//! and `llama-server` on this machine. Nothing is sent anywhere until the user has
//! chosen a model and, for the cloud ones, agreed to what leaves the machine (AD-4).
//! Every
//! prompt is built from reduced episodes, so an executable path or a URL query string
//! cannot reach a provider even by mistake, and a private session is described as time
//! in an application and nothing more.
//!
//! The local provider manages a `llama-server` child process and unloads it when it
//! has been idle (AD-3). The model is never held resident for a background utility.

pub mod anthropic;
pub mod catalog;
pub mod download;
pub mod google;
pub mod llama;
pub mod openai;
pub mod prompt;
pub mod provider;
pub mod runtime;
pub mod secrets;
pub mod service;

#[cfg(test)]
mod testing;

pub use anthropic::AnthropicProvider;
pub use catalog::{CatalogModel, ModelStatus, catalog};
pub use download::{Cancel, DownloadError, Progress, ProgressListener, fetch_model};
pub use google::GoogleProvider;
pub use llama::{LlamaOptions, LlamaServer, LlamaStatus};
pub use openai::OpenAiProvider;
pub use prompt::{Prompt, day_prompt, hour_prompt};
pub use provider::{Completion, InferenceError, Request};
pub use secrets::{SECRETS, Secret};
pub use service::{InferenceService, Readiness, RunReport};
