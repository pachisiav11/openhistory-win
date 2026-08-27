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

use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::json;

use crate::provider::{Completion, InferenceError, Request, Result, tidy};

pub const PROVIDER: &str = "google";

/// The published API root. Overridable so the tests can point at a local server.
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// Extra `maxOutputTokens` beyond what the summary itself needs, used only when the
/// model will not accept a thinking budget.
///
/// Gemini thinks before answering and counts those tokens against the same ceiling as
/// the answer. This headroom is a guess at how much the thinking will want, and a guess
/// is all it can be: nothing stops a long reasoning pass from spending the summary's
/// share as well as its own and returning a candidate with no text in it. That is what
/// produced empty day summaries. It survives as the fallback for a model that rejects
/// `thinkingConfig`; see [`GoogleProvider::complete`].
const THINKING_HEADROOM: u32 = 4000;

/// The bounds on what Gemini may spend thinking before it answers.
///
/// Asking for a budget explicitly is what makes the ceiling safe. The budget is what
/// the thinking may use, and the summary's own `max_tokens` are added on top of it, so
/// the answer keeps its full length however long the reasoning runs. Without it the two
/// compete for one allowance and the reasoning, which comes first, can take all of it.
///
/// Thinking is bounded here, not switched off. `thinkingBudget: 0` would fix the empty
/// summaries outright and is the wrong trade: the day summary asks for two paragraphs
/// of analysis, and reasoning is how Gemini arrives at them.
///
/// The floor is generous enough for an hour's two sentences and the ceiling is roughly
/// what a 500-word analysis of a full day has ever needed. Between them the budget
/// scales with the summary being asked for.
const MIN_THINKING_BUDGET: u32 = 1_024;
const MAX_THINKING_BUDGET: u32 = 8_192;

/// What to let Gemini spend thinking about a summary of `max_tokens`.
fn thinking_budget(max_tokens: u32) -> u32 {
    max_tokens
        .saturating_mul(2)
        .clamp(MIN_THINKING_BUDGET, MAX_THINKING_BUDGET)
}

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
        let (status, text) = match self
            .send(request, Some(thinking_budget(request.prompt.max_tokens)))
            .await
        {
            // A model that predates the thinking controls, or one the moving alias has
            // swung to that names them differently, rejects the field outright. Rather
            // than keep a list of which releases accept it, ask once and fall back to
            // the old unbounded headroom for the one that does not.
            Ok((status, text)) if status == StatusCode::BAD_REQUEST && refuses_thinking(&text) => {
                tracing::debug!(
                    model = %self.model,
                    "this Gemini model will not take a thinking budget; falling back to headroom"
                );
                self.send(request, None).await?
            }
            other => other?,
        };

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
            // Why nothing arrived matters, because the two causes want different
            // answers. A model that stopped at its ceiling was cut off mid-thought and
            // needs more room; a model that stopped normally had nothing to say.
            return Err(match parsed.finish_reason() {
                Some(reason) if reason.eq_ignore_ascii_case("MAX_TOKENS") => {
                    InferenceError::Truncated {
                        provider: PROVIDER,
                        reason: "it reached its token ceiling while still thinking and never                                  began the summary"
                            .to_owned(),
                    }
                }
                Some(reason) if !reason.eq_ignore_ascii_case("STOP") => {
                    InferenceError::Truncated {
                        provider: PROVIDER,
                        reason: format!("it stopped for {reason} with nothing written"),
                    }
                }
                _ => InferenceError::Empty { provider: PROVIDER },
            });
        }

        Ok(Completion {
            text: cleaned,
            provider: PROVIDER,
            model: parsed.model_version.unwrap_or_else(|| self.model.clone()),
        })
    }

    /// One request, with the thinking either bounded to a budget or left to the old
    /// unconditional headroom. The status is returned rather than judged, so the caller
    /// can decide whether a rejection is worth a second attempt.
    async fn send(&self, request: &Request, thinking: Option<u32>) -> Result<(StatusCode, String)> {
        let mut generation = json!({
            "maxOutputTokens": request
                .prompt
                .max_tokens
                .saturating_add(thinking.unwrap_or(THINKING_HEADROOM)),
        });
        if let Some(budget) = thinking {
            generation["thinkingConfig"] = json!({ "thinkingBudget": budget });
        }

        let body = json!({
            "systemInstruction": { "parts": [{ "text": request.prompt.system }] },
            "contents": [{
                "role": "user",
                "parts": [{ "text": request.prompt.user }],
            }],
            "generationConfig": generation,
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

        Ok((status, text))
    }
}

/// Whether a 400 is the model saying it does not know what a thinking budget is.
fn refuses_thinking(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("thinking") || body.contains("thinkingconfig") || body.contains("thinkingbudget")
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

    /// Why the first candidate stopped. Read only when the text came back empty, where
    /// it is the difference between a model that was cut off and one that had nothing.
    fn finish_reason(&self) -> Option<&str> {
        self.candidates
            .first()
            .and_then(|candidate| candidate.finish_reason.as_deref())
    }

    fn block_reason(&self) -> Option<&str> {
        self.prompt_feedback
            .as_ref()
            .and_then(|feedback| feedback.block_reason.as_deref())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    #[serde(default)]
    content: Option<Content>,
    /// `STOP` when the model finished, `MAX_TOKENS` when it ran out of allowance,
    /// `SAFETY` and others when it was stopped.
    #[serde(default)]
    finish_reason: Option<String>,
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

    /// The empty summaries this fixes. Gemini reasons on the same allowance it answers
    /// on, so the budget has to be asked for and the summary's tokens added on top of
    /// it — otherwise the reasoning, which runs first, can spend the lot.
    #[tokio::test]
    async fn the_thinking_is_given_a_budget_and_the_summary_is_paid_for_on_top_of_it() {
        let server = FakeHttp::serving(200, &answer("Fine.")).await;
        provider(server.base_url())
            .complete(&request())
            .await
            .unwrap();

        let sent = server.last_request();
        // 300 tokens of summary, so 1,024 of thinking: the floor, since twice 300 is
        // under it.
        assert!(sent.contains(r#""thinkingBudget":1024"#), "{sent}");
        assert!(sent.contains(r#""maxOutputTokens":1324"#), "{sent}");
    }

    #[tokio::test]
    async fn the_budget_scales_with_the_summary_and_stops_at_the_ceiling() {
        assert_eq!(thinking_budget(300), 1_024);
        assert_eq!(thinking_budget(1_200), 2_400);
        assert_eq!(thinking_budget(9_000), 8_192);
    }

    /// Thinking is bounded, never switched off: it is what the day summary's analysis
    /// is written from.
    #[tokio::test]
    async fn the_budget_is_never_zero() {
        for tokens in [0, 1, 300, 1_200, 100_000] {
            assert!(thinking_budget(tokens) >= MIN_THINKING_BUDGET);
        }
    }

    /// The alias moves between Flash releases, and one it moves to may not know the
    /// field. Asking is cheap; keeping a table of which releases accept it is not.
    #[tokio::test]
    async fn a_model_that_will_not_take_a_budget_is_asked_again_without_one() {
        let refusal =
            r#"{"error":{"message":"Unknown name \"thinkingConfig\" at generationConfig"}}"#;
        let server = FakeHttp::scripted(vec![(400, refusal), (200, &answer("Fine."))]).await;

        let completion = provider(server.base_url())
            .complete(&request())
            .await
            .unwrap();

        assert_eq!(completion.text, "Fine.");
        assert_eq!(server.request_count(), 2);
        let second = server.last_request();
        assert!(!second.contains("thinkingBudget"), "{second}");
        assert!(second.contains(r#""maxOutputTokens":4300"#), "{second}");
    }

    /// A 400 about anything else is the error it says it is, and asking again without
    /// the budget would only bury it.
    #[tokio::test]
    async fn a_rejection_that_is_not_about_thinking_is_not_retried() {
        let refusal = r#"{"error":{"message":"API key not valid"}}"#;
        let server = FakeHttp::scripted(vec![(400, refusal), (200, &answer("Fine."))]).await;

        let error = provider(server.base_url())
            .complete(&request())
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            InferenceError::Rejected { status: 400, .. }
        ));
        assert_eq!(server.request_count(), 1);
    }

    /// "the model had nothing to say" was what a run that spent its whole allowance
    /// thinking used to report. It says which of the two it was now.
    #[tokio::test]
    async fn a_model_cut_off_before_it_answered_says_so() {
        let cut_off = r#"{"candidates":[{"finishReason":"MAX_TOKENS","content":{"parts":[]}}]}"#;
        let server = FakeHttp::serving(200, cut_off).await;

        let error = provider(server.base_url())
            .complete(&request())
            .await
            .unwrap_err();

        let InferenceError::Truncated { reason, .. } = &error else {
            panic!("expected a truncation, got {error}");
        };
        assert!(reason.contains("token ceiling"), "{reason}");
    }

    #[tokio::test]
    async fn a_model_that_stopped_normally_with_nothing_is_still_empty() {
        let nothing = r#"{"candidates":[{"finishReason":"STOP","content":{"parts":[]}}]}"#;
        let server = FakeHttp::serving(200, nothing).await;

        let error = provider(server.base_url())
            .complete(&request())
            .await
            .unwrap_err();

        assert!(matches!(error, InferenceError::Empty { .. }), "{error}");
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
