//! The OpenAI Responses API.
//!
//! The GPT-5.6 tiers — Luna, Terra and Sol — are served through `POST /v1/responses`
//! rather than the older chat endpoint. The request carries the system text as
//! `instructions` and the prompt as `input`, and the answer arrives as a list of
//! output items, of which only the `output_text` parts are the summary. Reasoning
//! items are in that same list and are skipped.
//!
//! **This provider is never exercised against the real API by the test suite**, for
//! the same reason as the Anthropic one: it would need a key. See AD-7.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::provider::{Completion, InferenceError, Request, Result, tidy};

pub const PROVIDER: &str = "openai";

/// The published API root. Overridable so the tests can point at a local server.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// Extra `max_output_tokens` allowed for a model that reasons before it answers.
///
/// Reasoning tokens are billed as output and counted against this ceiling. Without
/// headroom the limit is reached during reasoning and the response comes back
/// incomplete, with no text in it at all.
const REASONING_HEADROOM: u32 = 4000;

pub struct OpenAiProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAiProvider {
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

        Ok(OpenAiProvider {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key,
            model: model.into(),
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn reasons(&self) -> bool {
        oh_core::cloud_model(&self.model).is_some_and(|choice| choice.supports_effort)
    }

    pub async fn complete(&self, request: &Request) -> Result<Completion> {
        let wanted = request.prompt.max_tokens;
        let mut body = json!({
            "model": self.model,
            "instructions": request.prompt.system,
            "input": request.prompt.user,
            "max_output_tokens": if self.reasons() { wanted + REASONING_HEADROOM } else { wanted },
        });

        // Every GPT-5.6 tier reasons by default, and describing twenty window titles
        // does not call for it. `low` rather than `none` because the lower settings
        // are not accepted on every tier.
        if self.reasons() {
            body["reasoning"] = json!({ "effort": "low" });
        }

        let response = self
            .client
            .post(format!("{}/v1/responses", self.base_url))
            .bearer_auth(&self.api_key)
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

        let parsed: ResponsePayload = serde_json::from_str(&text).map_err(|error| {
            InferenceError::Transport(format!("could not read the OpenAI response: {error}"))
        })?;

        let cleaned = tidy(&parsed.text());
        if cleaned.is_empty() {
            // A run that stopped on the token ceiling has an empty output rather than
            // an error status, and saying so is more use than "the model said nothing".
            if parsed.incomplete_reason().is_some() {
                return Err(InferenceError::Rejected {
                    provider: PROVIDER,
                    status: status.as_u16(),
                    message: "the answer was cut off before any text was written; \
                              the reasoning used the whole output budget"
                        .to_owned(),
                });
            }
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
struct ResponsePayload {
    #[serde(default)]
    model: Option<String>,
    /// The convenience field the SDKs expose. Present on some responses, absent on
    /// others, so it is a shortcut and not the only path.
    #[serde(default)]
    output_text: Option<String>,
    #[serde(default)]
    output: Vec<OutputItem>,
    #[serde(default)]
    incomplete_details: Option<Incomplete>,
}

impl ResponsePayload {
    /// The assistant's text, from wherever this response carries it.
    fn text(&self) -> String {
        if let Some(shortcut) = self
            .output_text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return shortcut.to_owned();
        }

        self.output
            .iter()
            .flat_map(|item| item.content.iter())
            .filter(|part| part.kind == "output_text")
            .filter_map(|part| part.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn incomplete_reason(&self) -> Option<&str> {
        self.incomplete_details
            .as_ref()
            .and_then(|details| details.reason.as_deref())
    }
}

#[derive(Deserialize)]
struct OutputItem {
    #[serde(default)]
    content: Vec<ContentPart>,
}

#[derive(Deserialize)]
struct ContentPart {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct Incomplete {
    #[serde(default)]
    reason: Option<String>,
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

    fn answer(text: &str) -> String {
        format!(
            r#"{{"model":"gpt-5.6-luna","output":[{{"type":"message","content":[{{"type":"output_text","text":"{text}"}}]}}]}}"#
        )
    }

    #[tokio::test]
    async fn a_successful_call_returns_the_text_and_the_model_that_wrote_it() {
        let server = FakeHttp::serving(200, &answer("You worked on the collector.")).await;
        let provider =
            OpenAiProvider::with_base_url(server.base_url(), "sk-test", "gpt-5.6-luna").unwrap();

        let completion = provider.complete(&request()).await.unwrap();
        assert_eq!(completion.text, "You worked on the collector.");
        assert_eq!(completion.provider, "openai");
        assert_eq!(completion.model, "gpt-5.6-luna");
    }

    #[tokio::test]
    async fn the_request_carries_the_key_the_prompt_and_the_endpoint() {
        let server = FakeHttp::serving(200, &answer("Fine.")).await;
        let provider =
            OpenAiProvider::with_base_url(server.base_url(), "sk-secret", "gpt-5.6-luna").unwrap();
        provider.complete(&request()).await.unwrap();

        let sent = server.last_request();
        assert!(sent.contains("POST /v1/responses"), "{sent}");
        assert!(sent.contains("Bearer sk-secret"), "{sent}");
        assert!(sent.contains("You summarize."), "{sent}");
        assert!(sent.contains("What happened?"), "{sent}");
    }

    #[tokio::test]
    async fn a_reasoning_model_asks_for_low_effort_and_room_to_use_it() {
        let server = FakeHttp::serving(200, &answer("Fine.")).await;
        let provider =
            OpenAiProvider::with_base_url(server.base_url(), "sk-test", "gpt-5.6-sol").unwrap();
        provider.complete(&request()).await.unwrap();

        let sent = server.last_request();
        assert!(sent.contains(r#""effort":"low""#), "{sent}");
        assert!(sent.contains(r#""max_output_tokens":4300"#), "{sent}");
    }

    #[tokio::test]
    async fn an_unknown_model_is_sent_as_written_with_no_extra_shaping() {
        let server = FakeHttp::serving(200, &answer("Fine.")).await;
        let provider =
            OpenAiProvider::with_base_url(server.base_url(), "sk-test", "gpt-9-imaginary").unwrap();
        provider.complete(&request()).await.unwrap();

        let sent = server.last_request();
        assert!(!sent.contains("effort"), "{sent}");
        assert!(sent.contains(r#""max_output_tokens":300"#), "{sent}");
    }

    #[tokio::test]
    async fn the_shortcut_field_is_used_when_the_response_carries_one() {
        let server = FakeHttp::serving(200, r#"{"output_text":"A short day."}"#).await;
        let provider =
            OpenAiProvider::with_base_url(server.base_url(), "sk-test", "gpt-5.6-luna").unwrap();

        assert_eq!(
            provider.complete(&request()).await.unwrap().text,
            "A short day."
        );
    }

    #[tokio::test]
    async fn reasoning_items_are_not_mistaken_for_the_answer() {
        let body = r#"{"output":[
            {"type":"reasoning","content":[{"type":"reasoning_text","text":"Let me think about this."}]},
            {"type":"message","content":[{"type":"output_text","text":"A quiet morning."}]}
        ]}"#;
        let server = FakeHttp::serving(200, body).await;
        let provider =
            OpenAiProvider::with_base_url(server.base_url(), "sk-test", "gpt-5.6-luna").unwrap();

        let completion = provider.complete(&request()).await.unwrap();
        assert_eq!(completion.text, "A quiet morning.");
    }

    #[tokio::test]
    async fn a_run_that_used_its_whole_budget_reasoning_says_so() {
        let body = r#"{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[]}"#;
        let server = FakeHttp::serving(200, body).await;
        let provider =
            OpenAiProvider::with_base_url(server.base_url(), "sk-test", "gpt-5.6-sol").unwrap();

        let error = provider.complete(&request()).await.err().unwrap();
        assert!(error.to_string().contains("cut off"), "{error}");
    }

    #[tokio::test]
    async fn a_rejection_carries_the_reason_the_api_gave() {
        let server = FakeHttp::serving(
            429,
            r#"{"error":{"message":"Rate limit reached for gpt-5.6-luna"}}"#,
        )
        .await;
        let provider =
            OpenAiProvider::with_base_url(server.base_url(), "sk-test", "gpt-5.6-luna").unwrap();

        let error = provider.complete(&request()).await.err().unwrap();
        match error {
            InferenceError::Rejected {
                status,
                ref message,
                ..
            } => {
                assert_eq!(status, 429);
                assert!(message.contains("Rate limit"), "{message}");
            }
            other => panic!("expected a rejection, got {other}"),
        }
        assert!(error.is_transient());
    }

    #[tokio::test]
    async fn an_empty_answer_is_an_error_rather_than_an_empty_summary() {
        let server = FakeHttp::serving(200, r#"{"output":[]}"#).await;
        let provider =
            OpenAiProvider::with_base_url(server.base_url(), "sk-test", "gpt-5.6-luna").unwrap();

        assert!(matches!(
            provider.complete(&request()).await.err().unwrap(),
            InferenceError::Empty { .. }
        ));
    }

    #[test]
    fn a_missing_key_is_refused_before_anything_is_sent() {
        assert!(matches!(
            OpenAiProvider::new("   ", "gpt-5.6-luna").err(),
            Some(InferenceError::NoApiKey)
        ));
    }
}
