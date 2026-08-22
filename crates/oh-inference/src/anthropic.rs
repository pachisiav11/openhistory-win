//! The Anthropic Messages API.
//!
//! Called directly over HTTP rather than through the SDK: the request is one JSON
//! object with three fields and the response is one more, and the whole client is
//! shorter than the code needed to bridge an async SDK into this crate's error type.
//!
//! **This provider is never exercised against the real API by the test suite.** The
//! tests run against a local server that speaks the same shapes, which verifies the
//! request that is built, the answer that is parsed, and every failure path — but not
//! that the live endpoint agrees. That gap is deliberate and recorded in AD-7; it
//! would need an API key, and asking the user to paste a secret into a chat window is
//! not something this project does.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::provider::{Completion, InferenceError, Request, Result, tidy};

pub const PROVIDER: &str = "anthropic";

/// The published API root. Overridable so the tests can point at a local server.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// The version header the Messages API requires.
const API_VERSION: &str = "2023-06-01";

/// Extra `max_tokens` allowed for a model that thinks before it answers.
const THINKING_HEADROOM: u32 = 4000;

pub struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        Self::with_base_url(DEFAULT_BASE_URL, api_key, model)
    }

    pub fn with_base_url(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(InferenceError::NoApiKey);
        }

        let client = reqwest::Client::builder()
            .user_agent(concat!("openhistory-win/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| InferenceError::Transport(error.to_string()))?;

        Ok(AnthropicProvider {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key,
            model: model.into(),
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// True when the chosen model reasons before answering.
    fn thinks(&self) -> bool {
        oh_core::cloud_model(&self.model).is_some_and(|choice| choice.supports_effort)
    }

    /// `max_tokens` for the request.
    ///
    /// Thinking tokens are counted against this ceiling, so a model that thinks needs
    /// headroom above what the summary itself will take. Without it a three-sentence
    /// summary can be cut off before it starts.
    fn token_budget(&self, wanted: u32) -> u32 {
        if self.thinks() {
            wanted + THINKING_HEADROOM
        } else {
            wanted
        }
    }

    /// Generate one summary.
    pub async fn complete(&self, request: &Request) -> Result<Completion> {
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.token_budget(request.prompt.max_tokens),
            "system": request.prompt.system,
            "messages": [{ "role": "user", "content": request.prompt.user }],
        });

        // Sonnet and Opus think adaptively unless told otherwise, and a summary of
        // twenty window titles is not a task that needs it. Asking for the lowest
        // effort is the supported way to say so; disabling thinking outright is
        // rejected at some effort levels and misbehaves at others.
        if self.thinks() {
            body["output_config"] = json!({ "effort": "low" });
        }

        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .timeout(request.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|error| classify(error, request.timeout))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| InferenceError::Transport(error.to_string()))?;

        if !status.is_success() {
            return Err(InferenceError::Rejected {
                provider: PROVIDER,
                status: status.as_u16(),
                message: describe_error(&text),
            });
        }

        let parsed: MessageResponse = serde_json::from_str(&text).map_err(|error| {
            InferenceError::Transport(format!("could not read the Anthropic response: {error}"))
        })?;

        let joined: String = parsed
            .content
            .iter()
            .filter(|block| block.kind == "text")
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        let cleaned = tidy(&joined);
        if cleaned.is_empty() {
            return Err(InferenceError::Empty { provider: PROVIDER });
        }

        Ok(Completion {
            text: cleaned,
            provider: PROVIDER,
            model: parsed.model.unwrap_or_else(|| self.model.clone()),
        })
    }
}

fn classify(error: reqwest::Error, timeout: Duration) -> InferenceError {
    if error.is_timeout() {
        return InferenceError::TimedOut {
            provider: PROVIDER,
            seconds: timeout.as_secs(),
        };
    }
    InferenceError::Transport(error.to_string())
}

/// Pull the human-readable part out of an error body, falling back to the body itself.
fn describe_error(body: &str) -> String {
    #[derive(Deserialize)]
    struct ApiError {
        error: Option<Detail>,
    }
    #[derive(Deserialize)]
    struct Detail {
        message: Option<String>,
    }

    serde_json::from_str::<ApiError>(body)
        .ok()
        .and_then(|parsed| parsed.error)
        .and_then(|detail| detail.message)
        .unwrap_or_else(|| {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                "no detail was given".to_owned()
            } else {
                trimmed.chars().take(400).collect()
            }
        })
}

#[derive(Deserialize)]
struct MessageResponse {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::Prompt;
    use crate::testing::FakeHttp;

    fn request() -> Request {
        Request::cloud(Prompt {
            system: "You summarize.".into(),
            user: "What happened?".into(),
            max_tokens: 300,
        })
    }

    #[tokio::test]
    async fn a_successful_call_returns_the_text_and_the_model_that_wrote_it() {
        let server = FakeHttp::serving(
            200,
            r#"{"model":"claude-haiku-4-5-20251001","content":[{"type":"text","text":"You worked on the collector."}]}"#,
        )
        .await;

        let provider =
            AnthropicProvider::with_base_url(server.base_url(), "sk-ant-test", "claude-haiku-4-5")
                .unwrap();
        let completion = provider.complete(&request()).await.unwrap();

        assert_eq!(completion.text, "You worked on the collector.");
        assert_eq!(completion.provider, "anthropic");
        assert_eq!(completion.model, "claude-haiku-4-5-20251001");
    }

    #[tokio::test]
    async fn the_request_carries_the_key_the_version_and_the_prompt() {
        let server =
            FakeHttp::serving(200, r#"{"content":[{"type":"text","text":"Fine."}]}"#).await;
        let provider =
            AnthropicProvider::with_base_url(server.base_url(), "sk-ant-secret", "some-model")
                .unwrap();
        provider.complete(&request()).await.unwrap();

        let seen = server.last_request();
        assert!(seen.contains("POST /v1/messages"), "{seen}");
        assert!(seen.contains("x-api-key: sk-ant-secret"), "{seen}");
        assert!(seen.contains("anthropic-version: 2023-06-01"), "{seen}");
        assert!(seen.contains("\"system\":\"You summarize.\""), "{seen}");
        assert!(seen.contains("\"max_tokens\":300"), "{seen}");
        assert!(seen.contains("\"model\":\"some-model\""), "{seen}");
    }

    #[tokio::test]
    async fn an_api_error_reports_the_status_and_the_message() {
        let server = FakeHttp::serving(
            401,
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
        )
        .await;

        let provider =
            AnthropicProvider::with_base_url(server.base_url(), "sk-ant-wrong", "some-model")
                .unwrap();
        let error = provider.complete(&request()).await.unwrap_err();

        match error {
            InferenceError::Rejected {
                status,
                ref message,
                ..
            } => {
                assert_eq!(status, 401);
                assert_eq!(message, "invalid x-api-key");
            }
            other => panic!("expected a rejection, got {other:?}"),
        }
        assert!(!error.is_transient());
    }

    #[tokio::test]
    async fn a_rate_limit_is_reported_as_worth_retrying() {
        let server = FakeHttp::serving(429, r#"{"error":{"message":"rate limited"}}"#).await;
        let provider =
            AnthropicProvider::with_base_url(server.base_url(), "sk-ant-test", "m").unwrap();

        assert!(
            provider
                .complete(&request())
                .await
                .unwrap_err()
                .is_transient()
        );
    }

    #[tokio::test]
    async fn an_answer_with_no_text_is_an_error_rather_than_an_empty_summary() {
        let server = FakeHttp::serving(200, r#"{"content":[]}"#).await;
        let provider =
            AnthropicProvider::with_base_url(server.base_url(), "sk-ant-test", "m").unwrap();

        assert!(matches!(
            provider.complete(&request()).await.unwrap_err(),
            InferenceError::Empty { .. }
        ));
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_a_transport_failure_not_a_panic() {
        let server = FakeHttp::serving(200, "<html>gateway</html>").await;
        let provider =
            AnthropicProvider::with_base_url(server.base_url(), "sk-ant-test", "m").unwrap();

        assert!(matches!(
            provider.complete(&request()).await.unwrap_err(),
            InferenceError::Transport(_)
        ));
    }

    #[tokio::test]
    async fn thinking_blocks_are_skipped_and_text_blocks_are_joined() {
        let server = FakeHttp::serving(
            200,
            r#"{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"First."},{"type":"text","text":"Second."}]}"#,
        )
        .await;
        let provider =
            AnthropicProvider::with_base_url(server.base_url(), "sk-ant-test", "m").unwrap();

        assert_eq!(
            provider.complete(&request()).await.unwrap().text,
            "First. Second."
        );
    }

    #[tokio::test]
    async fn a_thinking_model_is_asked_for_low_effort_and_given_headroom() {
        let server =
            FakeHttp::serving(200, r#"{"content":[{"type":"text","text":"Fine."}]}"#).await;
        let provider =
            AnthropicProvider::with_base_url(server.base_url(), "sk-ant-test", "claude-opus-5")
                .unwrap();
        provider.complete(&request()).await.unwrap();

        let sent = server.last_request();
        assert!(sent.contains(r#""effort":"low""#), "{sent}");
        assert!(sent.contains(r#""max_tokens":4300"#), "{sent}");
    }

    #[tokio::test]
    async fn haiku_is_asked_for_neither_effort_nor_headroom() {
        let server =
            FakeHttp::serving(200, r#"{"content":[{"type":"text","text":"Fine."}]}"#).await;
        let provider =
            AnthropicProvider::with_base_url(server.base_url(), "sk-ant-test", "claude-haiku-4-5")
                .unwrap();
        provider.complete(&request()).await.unwrap();

        let sent = server.last_request();
        assert!(!sent.contains("effort"), "{sent}");
        assert!(sent.contains(r#""max_tokens":300"#), "{sent}");
    }

    #[tokio::test]
    async fn a_model_named_by_hand_is_sent_as_a_plain_request() {
        let server =
            FakeHttp::serving(200, r#"{"content":[{"type":"text","text":"Fine."}]}"#).await;
        let provider =
            AnthropicProvider::with_base_url(server.base_url(), "sk-ant-test", "claude-opus-4-8")
                .unwrap();
        provider.complete(&request()).await.unwrap();

        let sent = server.last_request();
        assert!(!sent.contains("effort"), "{sent}");
        assert!(sent.contains(r#""model":"claude-opus-4-8""#), "{sent}");
    }

    #[test]
    fn a_blank_key_is_refused_before_any_request_is_made() {
        assert!(matches!(
            AnthropicProvider::new("   ", "m").err(),
            Some(InferenceError::NoApiKey)
        ));
    }

    #[test]
    fn an_error_body_that_is_not_json_still_produces_a_message() {
        assert_eq!(
            describe_error("upstream connect error"),
            "upstream connect error"
        );
        assert_eq!(describe_error("   "), "no detail was given");
    }
}
