//! The Google AI Studio Gemini API.
//!
//! `POST /v1beta/models/{model}:generateContent`. The key goes in the `x-goog-api-key`
//! header rather than the `?key=` query parameter the quickstarts use: a query string
//! ends up in proxy logs and crash reports, and a header does not.
//!
//! `gemini-flash-latest` is a moving alias. Google hot-swaps what stands behind it with
//! each Flash release, with two weeks' notice before a breaking change. For a
//! two-sentence summary that is the right trade, and it is why the catalog carries the
//! alias rather than a dated identifier.
//!
//! **This provider is never exercised against the real API by the test suite.** See
//! AD-7.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::provider::{Completion, InferenceError, Request, Result, tidy};

pub const PROVIDER: &str = "google";

/// The published API root. Overridable so the tests can point at a local server.
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// Extra `maxOutputTokens` beyond what the summary itself needs.
///
/// Gemini thinks before answering and counts those tokens against this ceiling. The
/// headroom is unconditional here because the thinking controls differ between Flash
/// releases and the alias can move under us; paying for a few thousand tokens of
/// headroom is cheaper than a summary that arrives truncated.
const THINKING_HEADROOM: u32 = 4000;

pub struct GoogleProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl GoogleProvider {
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

        Ok(GoogleProvider {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key,
            model: model.into(),
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub async fn complete(&self, request: &Request) -> Result<Completion> {
        let body = json!({
            "systemInstruction": { "parts": [{ "text": request.prompt.system }] },
            "contents": [{
                "role": "user",
                "parts": [{ "text": request.prompt.user }],
            }],
            "generationConfig": {
                "maxOutputTokens": request.prompt.max_tokens + THINKING_HEADROOM,
            },
        });

        let response = self
            .client
            .post(format!(
                "{}/v1beta/models/{}:generateContent",
                self.base_url, self.model
            ))
            .header("x-goog-api-key", &self.api_key)
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

        let parsed: GenerateResponse = serde_json::from_str(&text).map_err(|error| {
            InferenceError::Transport(format!("could not read the Gemini response: {error}"))
        })?;

        // A prompt the safety filters stopped comes back with a 200 and no candidates,
        // which would otherwise read as "the model had nothing to say".
        if let Some(reason) = parsed.block_reason() {
            return Err(InferenceError::Rejected {
                provider: PROVIDER,
                status: status.as_u16(),
                message: format!("the prompt was refused: {reason}"),
            });
        }

        let cleaned = tidy(&parsed.text());
        if cleaned.is_empty() {
            return Err(InferenceError::Empty { provider: PROVIDER });
        }

        Ok(Completion {
            text: cleaned,
            provider: PROVIDER,
            model: parsed.model_version.unwrap_or_else(|| self.model.clone()),
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
#[serde(rename_all = "camelCase")]
struct GenerateResponse {
    #[serde(default)]
    model_version: Option<String>,
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default)]
    prompt_feedback: Option<PromptFeedback>,
}

impl GenerateResponse {
    fn text(&self) -> String {
        self.candidates
            .iter()
            .flat_map(|candidate| candidate.content.iter())
            .flat_map(|content| content.parts.iter())
            // A thinking part carries `thought: true` and is not the answer.
            .filter(|part| !part.thought)
            .filter_map(|part| part.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn block_reason(&self) -> Option<&str> {
        self.prompt_feedback
            .as_ref()
            .and_then(|feedback| feedback.block_reason.as_deref())
    }
}

#[derive(Deserialize)]
struct Candidate {
    #[serde(default)]
    content: Option<Content>,
}

#[derive(Deserialize)]
struct Content {
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Deserialize)]
struct Part {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thought: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptFeedback {
    #[serde(default)]
    block_reason: Option<String>,
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
            r#"{{"modelVersion":"gemini-3.7-flash","candidates":[{{"content":{{"parts":[{{"text":"{text}"}}]}}}}]}}"#
        )
    }

    fn provider(base: String) -> GoogleProvider {
        GoogleProvider::with_base_url(base, "AIza-test", "gemini-flash-latest").unwrap()
    }

    #[tokio::test]
    async fn a_successful_call_returns_the_text_and_the_build_that_answered() {
        let server = FakeHttp::serving(200, &answer("You worked on the collector.")).await;
        let completion = provider(server.base_url())
            .complete(&request())
            .await
            .unwrap();

        assert_eq!(completion.text, "You worked on the collector.");
        assert_eq!(completion.provider, "google");
        // The alias moves, so the summary records which build actually wrote it.
        assert_eq!(completion.model, "gemini-3.7-flash");
    }

    #[tokio::test]
    async fn the_key_travels_in_a_header_and_never_in_the_url() {
        let server = FakeHttp::serving(200, &answer("Fine.")).await;
        provider(server.base_url())
            .complete(&request())
            .await
            .unwrap();

        let sent = server.last_request();
        assert!(
            sent.contains("POST /v1beta/models/gemini-flash-latest:generateContent"),
            "{sent}"
        );
        assert!(sent.contains("x-goog-api-key: AIza-test"), "{sent}");
        assert!(!sent.contains("key=AIza-test"), "{sent}");
    }

    #[tokio::test]
    async fn the_system_text_goes_in_the_field_meant_for_it() {
        let server = FakeHttp::serving(200, &answer("Fine.")).await;
        provider(server.base_url())
            .complete(&request())
            .await
            .unwrap();

        let sent = server.last_request();
        assert!(sent.contains("systemInstruction"), "{sent}");
        assert!(sent.contains("You summarize."), "{sent}");
        assert!(sent.contains(r#""maxOutputTokens":4300"#), "{sent}");
    }

    #[tokio::test]
    async fn a_thinking_part_is_not_mistaken_for_the_answer() {
        let body = r#"{"candidates":[{"content":{"parts":[
            {"text":"Considering the titles.","thought":true},
            {"text":"A quiet morning of Rust."}
        ]}}]}"#;
        let server = FakeHttp::serving(200, body).await;

        let completion = provider(server.base_url())
            .complete(&request())
            .await
            .unwrap();
        assert_eq!(completion.text, "A quiet morning of Rust.");
    }

    #[tokio::test]
    async fn a_refused_prompt_says_why_rather_than_looking_empty() {
        let body = r#"{"promptFeedback":{"blockReason":"SAFETY"},"candidates":[]}"#;
        let server = FakeHttp::serving(200, body).await;

        let error = provider(server.base_url())
            .complete(&request())
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("SAFETY"), "{error}");
    }

    #[tokio::test]
    async fn a_rejection_carries_the_reason_the_api_gave() {
        let server = FakeHttp::serving(
            400,
            r#"{"error":{"message":"API key not valid. Please pass a valid API key."}}"#,
        )
        .await;

        let error = provider(server.base_url())
            .complete(&request())
            .await
            .err()
            .unwrap();
        match error {
            InferenceError::Rejected {
                status,
                ref message,
                ..
            } => {
                assert_eq!(status, 400);
                assert!(message.contains("API key not valid"), "{message}");
            }
            other => panic!("expected a rejection, got {other}"),
        }
        assert!(!error.is_transient());
    }

    #[tokio::test]
    async fn an_empty_answer_is_an_error_rather_than_an_empty_summary() {
        let server = FakeHttp::serving(200, r#"{"candidates":[]}"#).await;

        assert!(matches!(
            provider(server.base_url())
                .complete(&request())
                .await
                .err()
                .unwrap(),
            InferenceError::Empty { .. }
        ));
    }

    #[test]
    fn a_missing_key_is_refused_before_anything_is_sent() {
        assert!(matches!(
            GoogleProvider::new("", "gemini-flash-latest").err(),
            Some(InferenceError::NoApiKey)
        ));
    }
}
