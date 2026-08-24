//! What a summarizer has to be able to do, and how it can fail.
//!
//! There are exactly two providers and there is no plan for a third, so this is an
//! enum rather than a trait object. Dynamic dispatch over an async trait would need a
//! dependency and a lifetime dance to express something a two-arm `match` says plainly.

use std::time::Duration;

use crate::prompt::Prompt;

/// How long a single generation may take before it is abandoned.
///
/// Cloud and local differ by an order of magnitude: the plan's gate allows 5 seconds
/// for Anthropic and 30 for llama.cpp, and a local model on a machine without a usable
/// GPU can exceed even that on a first, cold generation.
pub const CLOUD_TIMEOUT: Duration = Duration::from_secs(60);
pub const LOCAL_TIMEOUT: Duration = Duration::from_secs(300);

/// How long Google is given, which is longer than the other two clouds.
///
/// Gemini reasons before it answers, and that reasoning is generated on the same
/// request as the summary — `google.rs` buys it 4,000 tokens of headroom that neither
/// Anthropic nor OpenAI is asked for. Sixty seconds is enough for the summary and not
/// reliably enough for the thinking that precedes it, which is how a working
/// configuration produced "google did not answer within 60s".
pub const GOOGLE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, thiserror::Error)]
pub enum InferenceError {
    /// No provider is selected, or the selected one is not configured.
    #[error("{0}")]
    NotConfigured(String),
    /// Cloud summarization is selected but the user has not agreed to it.
    #[error("cloud summarization has not been agreed to; nothing was sent")]
    ConsentMissing,
    /// No API key is stored for the cloud provider.
    #[error("no Anthropic API key is stored")]
    NoApiKey,
    /// The `llama-server` binary could not be found or would not start.
    #[error("the local model server could not start: {0}")]
    ServerUnavailable(String),
    /// The provider answered, and the answer was a refusal or an error.
    #[error("{provider} returned {status}: {message}")]
    Rejected {
        provider: &'static str,
        status: u16,
        message: String,
    },
    /// The provider did not answer in time.
    #[error("{provider} did not answer within {seconds}s")]
    TimedOut {
        provider: &'static str,
        seconds: u64,
    },
    /// The request could not be made, or the answer could not be read.
    #[error("{0}")]
    Transport(String),
    /// The provider answered with nothing usable.
    #[error("{provider} returned an empty summary")]
    Empty { provider: &'static str },
}

impl InferenceError {
    /// True when trying again later might work: a network blip, a busy server, a rate
    /// limit. A missing key or missing consent will not fix itself.
    pub fn is_transient(&self) -> bool {
        match self {
            InferenceError::Transport(_) | InferenceError::TimedOut { .. } => true,
            InferenceError::Rejected { status, .. } => *status == 429 || *status >= 500,
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, InferenceError>;

/// One generated summary, with enough provenance to render it honestly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub text: String,
    /// `anthropic` or `local`.
    pub provider: &'static str,
    /// The model that produced it: an API model name, or the GGUF file's stem.
    pub model: String,
}

/// Tidy a model's answer into the one paragraph that was asked for.
///
/// Small local models add a preamble however firmly the system prompt forbids one, and
/// some wrap the whole answer in quotes. Cleaning that here rather than in each
/// provider keeps the two answering the same shape.
pub fn tidy(raw: &str) -> String {
    let mut text = raw.trim();

    for opener in [
        "Here is a summary:",
        "Here's a summary:",
        "Here is the summary:",
        "Here's the summary:",
        "Summary:",
    ] {
        if let Some(rest) = text
            .strip_prefix(opener)
            .or_else(|| strip_prefix_ignoring_case(text, opener))
        {
            text = rest.trim_start();
        }
    }

    // A whole answer wrapped in quotes, not a quotation inside one.
    if text.len() > 1
        && text.starts_with('"')
        && text.ends_with('"')
        && text[1..text.len() - 1].find('"').is_none()
    {
        text = text[1..text.len() - 1].trim();
    }

    // Collapse the blank lines a model uses to separate paragraphs it was not asked
    // for, so a summary is one block of prose whatever it was given.
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_prefix_ignoring_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// A request as it goes to a provider.
#[derive(Debug, Clone)]
pub struct Request {
    pub prompt: Prompt,
    pub timeout: Duration,
}

impl Request {
    pub fn cloud(prompt: Prompt) -> Self {
        Request {
            prompt,
            timeout: CLOUD_TIMEOUT,
        }
    }

    pub fn local(prompt: Prompt) -> Self {
        Request {
            prompt,
            timeout: LOCAL_TIMEOUT,
        }
    }

    /// Give this request a different deadline from its kind's default.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preamble_is_removed() {
        assert_eq!(
            tidy("Here is a summary: You worked on the collector."),
            "You worked on the collector."
        );
        assert_eq!(
            tidy("SUMMARY: You worked on the collector."),
            "You worked on the collector."
        );
    }

    #[test]
    fn an_answer_wrapped_in_quotes_is_unwrapped() {
        assert_eq!(tidy("\"You worked on it.\""), "You worked on it.");
    }

    #[test]
    fn a_quotation_inside_the_answer_is_left_alone() {
        assert_eq!(
            tidy("You opened \"collector.rs\" and edited it."),
            "You opened \"collector.rs\" and edited it."
        );
    }

    #[test]
    fn paragraphs_are_joined_into_one_block() {
        assert_eq!(
            tidy("First sentence.\n\n  Second sentence.  \n"),
            "First sentence. Second sentence."
        );
    }

    #[test]
    fn a_rate_limit_is_worth_retrying_and_a_missing_key_is_not() {
        assert!(
            InferenceError::Rejected {
                provider: "anthropic",
                status: 429,
                message: String::new()
            }
            .is_transient()
        );
        assert!(
            InferenceError::Rejected {
                provider: "anthropic",
                status: 503,
                message: String::new()
            }
            .is_transient()
        );
        assert!(
            !InferenceError::Rejected {
                provider: "anthropic",
                status: 401,
                message: String::new()
            }
            .is_transient()
        );
        assert!(!InferenceError::NoApiKey.is_transient());
        assert!(!InferenceError::ConsentMissing.is_transient());
        assert!(InferenceError::Transport("reset".into()).is_transient());
    }
}
