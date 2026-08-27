//! Writing a day's summaries, and answering questions about one.
//!
//! [`InferenceService::summarize_day`] fills in the hours that do not have a summary
//! yet and then writes the day, and [`InferenceService::chat`] puts a question about a
//! day to the same model. Everything either needs comes from a [`DayReport`] and the
//! stored [`DaySummary`]; neither reads the event log.
//!
//! Two properties matter more than speed here:
//!
//! - **Nothing is regenerated without reason.** An hour that already has a summary is
//!   left alone unless the caller asks for a rewrite, or the hour has gained a minute
//!   or more of activity since — which is what happens to an hour summarized while it
//!   was still filling. Summaries cost money or a model load, so the test is whether
//!   the hour has actually changed, not whether time has passed.
//! - **A failure part-way through keeps what was written.** Every hour is saved as it
//!   completes, so a rate limit at 15:00 leaves the morning summarized rather than
//!   throwing the run away.

use anyhow::Result;
use chrono::NaiveDate;
use oh_core::summary::now;
use oh_core::{Config, DaySummary, HourSummary, InferenceProvider, SummaryStore};
use oh_processing::DayReport;
use oh_processing::attention;

use crate::anthropic::AnthropicProvider;
use crate::google::GoogleProvider;
use crate::llama::{LlamaOptions, LlamaServer, LlamaStatus};
use crate::openai::OpenAiProvider;
use crate::prompt::{ChatTurn, chat_prompt, day_prompt, episodes_in_hour, hour_prompt};
use crate::provider::{CLOUD_TIMEOUT, Completion, GOOGLE_TIMEOUT, InferenceError, Request};
use crate::secrets::{self, Secret};

/// What a summarization run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub date: String,
    /// Hours that were written on this run.
    pub hours_written: Vec<u32>,
    /// Hours skipped because they already had a summary.
    pub hours_skipped: Vec<u32>,
    /// Hours with too little activity to describe.
    pub hours_too_quiet: Vec<u32>,
    pub daily_written: bool,
    /// The first thing that went wrong, if anything did. The hours written before it
    /// are still saved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

impl RunReport {
    pub fn wrote_anything(&self) -> bool {
        !self.hours_written.is_empty() || self.daily_written
    }
}

/// Whether the provider can be used, and why not when it cannot.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Readiness {
    pub provider: String,
    pub ready: bool,
    /// Why summaries cannot be produced, in words the interface can show as-is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
    /// The model that would be used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// How many times one prompt is sent before the failure is reported.
///
/// Only what `InferenceError::is_transient` calls transient is tried again — a rate
/// limit, a 5xx, a dropped connection, a timeout. A missing key or a refused prompt
/// gives the same answer however many times it is asked, and asking again only spends
/// the user's quota to arrive at the same place.
const MAX_ATTEMPTS: u32 = 3;

/// Chooses a provider from the settings and writes summaries with it.
pub struct InferenceService {
    store: SummaryStore,
    llama: LlamaServer,
}

impl InferenceService {
    /// Open against the real summary directory.
    pub fn open() -> Result<Self> {
        Ok(InferenceService {
            store: SummaryStore::open()?,
            llama: LlamaServer::new(),
        })
    }

    pub fn in_dir(dir: impl Into<std::path::PathBuf>) -> Result<Self> {
        Ok(InferenceService {
            store: SummaryStore::in_dir(dir)?,
            llama: LlamaServer::new(),
        })
    }

    pub fn store(&self) -> &SummaryStore {
        &self.store
    }

    pub fn local_status(&self) -> LlamaStatus {
        self.llama.status()
    }

    /// Shut the local model server down, if one is running.
    pub async fn shutdown(&self) {
        self.llama.stop().await;
    }

    /// Whether summaries can be produced with these settings, and why not if they
    /// cannot.
    ///
    /// This is what the interface shows on the settings page and above an empty Day
    /// view. Every branch returns something a person can act on.
    pub fn readiness(&self, config: &Config) -> Readiness {
        let inference = &config.inference;
        let provider = inference.provider.as_str().to_owned();

        match inference.provider {
            InferenceProvider::Disabled => Readiness {
                provider,
                ready: false,
                blocked_by: Some("Summaries are off. Choose a provider in Settings.".into()),
                model: None,
            },
            cloud if cloud.is_cloud() => {
                let model = Some(inference.cloud_model.clone());
                if !inference.cloud_consent {
                    return Readiness {
                        provider,
                        ready: false,
                        blocked_by: Some(
                            "Cloud summaries need your agreement before anything is sent.".into(),
                        ),
                        model,
                    };
                }
                let Some(secret) = Secret::for_provider(cloud) else {
                    return Readiness {
                        provider,
                        ready: false,
                        blocked_by: Some("That provider cannot write summaries.".into()),
                        model,
                    };
                };
                if !secrets::is_stored(secret) {
                    return Readiness {
                        provider,
                        ready: false,
                        blocked_by: Some(format!("No {} is stored.", secret.label())),
                        model,
                    };
                }
                Readiness {
                    provider,
                    ready: true,
                    blocked_by: None,
                    model,
                }
            }
            _ => {
                let Some(path) = inference.local_model_path.as_ref() else {
                    return Readiness {
                        provider,
                        ready: false,
                        blocked_by: Some("No local model has been chosen.".into()),
                        model: None,
                    };
                };
                let model = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned());

                if !path.is_file() {
                    return Readiness {
                        provider,
                        ready: false,
                        blocked_by: Some(format!(
                            "The model file {} is missing. Download it again in Settings.",
                            path.display()
                        )),
                        model,
                    };
                }
                if local_binary(inference).is_none() {
                    return Readiness {
                        provider,
                        ready: false,
                        blocked_by: Some(
                            "llama-server has not been fetched yet. Settings will get \
                             it, or you can point at a copy you already have."
                                .into(),
                        ),
                        model,
                    };
                }
                Readiness {
                    provider,
                    ready: true,
                    blocked_by: None,
                    model,
                }
            }
        }
    }

    /// The stored summary for a day, whatever state it is in.
    pub fn summary(&self, date: NaiveDate) -> DaySummary {
        self.store.load(date)
    }

    /// Fill in every hour that has activity but no summary, then write the day.
    ///
    /// With `rewrite`, existing summaries are replaced instead of kept.
    pub async fn summarize_day(
        &self,
        config: &Config,
        report: &DayReport,
        rewrite: bool,
    ) -> Result<RunReport, InferenceError> {
        let date = NaiveDate::parse_from_str(&report.date, "%Y-%m-%d")
            .map_err(|_| InferenceError::NotConfigured(format!("{} is not a date", report.date)))?;

        self.require_ready(config)?;

        let mut summary = self.store.load(date);
        let mut run = RunReport {
            date: report.date.clone(),
            ..RunReport::default()
        };

        for hour in &report.rollup.hours {
            let written = summary.hour(hour.hour);
            if !rewrite && written.is_some_and(|written| !is_stale(written, hour)) {
                run.hours_skipped.push(hour.hour);
                continue;
            }

            let episodes = episodes_in_hour(report, hour);
            let Some(prompt) = hour_prompt(date, hour, &episodes) else {
                run.hours_too_quiet.push(hour.hour);
                continue;
            };

            match self.generate(config, prompt).await {
                Ok(completion) => {
                    summary.set_hour(HourSummary {
                        hour: hour.hour,
                        text: completion.text,
                        active_ms: hour.active_ms,
                        generated_at: now(),
                        provider: completion.provider.to_owned(),
                        model: completion.model,
                    });
                    // Saved as each hour completes, so a failure later keeps this.
                    self.save(&summary)?;
                    run.hours_written.push(hour.hour);
                }
                Err(error) => {
                    run.failure = Some(error.to_string());
                    return Ok(run);
                }
            }
        }

        // The day is rewritten whenever an hour changed, because it is a summary of
        // the hours and a stale one would contradict them.
        let day_is_stale = rewrite || !run.hours_written.is_empty() || summary.daily.is_none();
        if day_is_stale
            && let Some(prompt) = day_prompt(
                date,
                &report.rollup,
                &summary.hours,
                &attention::measure_all(&report.episodes),
            )
        {
            match self.generate(config, prompt).await {
                Ok(completion) => {
                    summary.set_daily(completion.text);
                    self.save(&summary)?;
                    run.daily_written = true;
                }
                Err(error) => run.failure = Some(error.to_string()),
            }
        }

        Ok(run)
    }

    /// Summarize one hour on its own, replacing whatever was there.
    pub async fn summarize_hour(
        &self,
        config: &Config,
        report: &DayReport,
        hour: u32,
    ) -> Result<HourSummary, InferenceError> {
        let date = NaiveDate::parse_from_str(&report.date, "%Y-%m-%d")
            .map_err(|_| InferenceError::NotConfigured(format!("{} is not a date", report.date)))?;

        let rolled = report
            .rollup
            .hours
            .iter()
            .find(|rolled| rolled.hour == hour)
            .ok_or_else(|| {
                InferenceError::NotConfigured(format!("nothing was recorded in hour {hour}"))
            })?;

        let episodes = episodes_in_hour(report, rolled);
        let prompt = hour_prompt(date, rolled, &episodes).ok_or_else(|| {
            InferenceError::NotConfigured(format!(
                "hour {hour} has too little activity to describe"
            ))
        })?;

        let completion = self.generate(config, prompt).await?;
        let written = HourSummary {
            hour,
            text: completion.text,
            active_ms: rolled.active_ms,
            generated_at: now(),
            provider: completion.provider.to_owned(),
            model: completion.model,
        };

        let mut summary = self.store.load(date);
        summary.set_hour(written.clone());
        self.save(&summary)?;
        Ok(written)
    }

    /// Answer one question about a day, from the day itself.
    ///
    /// The chat stands behind the summariser's readiness gate rather than one of its
    /// own, because it is the same model, the same key and the same consent: a question
    /// about a day is put to whatever writes that day's summaries.
    ///
    /// Nothing is stored. A summary is a document the person keeps and a conversation
    /// is not, so the transcript lives in the window that asked and the history comes
    /// back in with the next question.
    pub async fn chat(
        &self,
        config: &Config,
        report: &DayReport,
        turns: &[ChatTurn],
        question: &str,
    ) -> Result<Completion, InferenceError> {
        let question = question.trim();
        if question.is_empty() {
            return Err(InferenceError::NotConfigured(
                "there is no question to answer".into(),
            ));
        }

        let date = NaiveDate::parse_from_str(&report.date, "%Y-%m-%d")
            .map_err(|_| InferenceError::NotConfigured(format!("{} is not a date", report.date)))?;

        self.require_ready(config)?;

        let summary = self.store.load(date);
        let prompt = chat_prompt(date, report, &summary, turns, question);
        self.generate(config, prompt).await
    }

    /// Refuse early, and with the reason the interface would have shown, when nothing
    /// is configured to answer.
    fn require_ready(&self, config: &Config) -> Result<(), InferenceError> {
        let readiness = self.readiness(config);
        if readiness.ready {
            return Ok(());
        }

        Err(match config.inference.provider {
            cloud if cloud.is_cloud() && !config.inference.cloud_consent => {
                InferenceError::ConsentMissing
            }
            _ => InferenceError::NotConfigured(
                readiness
                    .blocked_by
                    .unwrap_or_else(|| "summaries are not configured".into()),
            ),
        })
    }

    /// Discard everything written about a day.
    pub fn forget(&self, date: NaiveDate) -> Result<(), InferenceError> {
        self.store
            .forget(date)
            .map_err(|error| InferenceError::Transport(error.to_string()))
    }

    fn save(&self, summary: &DaySummary) -> Result<(), InferenceError> {
        self.store
            .save(summary)
            .map_err(|error| InferenceError::Transport(error.to_string()))
    }

    /// Send one prompt, trying again when the reason it failed might not last.
    ///
    /// A summary is a background job with no one waiting on this exact attempt, so a
    /// blip is worth absorbing rather than reporting. The alternative is what the user
    /// saw: one slow answer from Google and the whole day's run stops with a message
    /// about sixty seconds.
    async fn generate(
        &self,
        config: &Config,
        prompt: crate::prompt::Prompt,
    ) -> Result<Completion, InferenceError> {
        let mut attempt = 1;
        loop {
            let outcome = self.generate_once(config, prompt.clone()).await;
            let Err(error) = outcome else {
                return outcome;
            };

            if !error.is_transient() || attempt >= MAX_ATTEMPTS {
                return Err(error);
            }

            tracing::warn!(
                attempt,
                of = MAX_ATTEMPTS,
                %error,
                "a summary attempt failed for a reason that may not last; trying again"
            );
            backoff(attempt).await;
            attempt += 1;
        }
    }

    /// Send one prompt to whichever provider is configured, once.
    async fn generate_once(
        &self,
        config: &Config,
        prompt: crate::prompt::Prompt,
    ) -> Result<Completion, InferenceError> {
        match config.inference.provider {
            InferenceProvider::Disabled => Err(InferenceError::NotConfigured(
                "no summarization provider is selected".into(),
            )),
            cloud if cloud.is_cloud() => {
                if !config.inference.cloud_consent {
                    return Err(InferenceError::ConsentMissing);
                }
                let secret = Secret::for_provider(cloud).ok_or_else(|| {
                    InferenceError::NotConfigured("that provider cannot write summaries".into())
                })?;
                let key = secrets::load(secret)
                    .map_err(|error| InferenceError::Transport(error.to_string()))?
                    .ok_or(InferenceError::NoApiKey)?;

                let model = &config.inference.cloud_model;
                let request = Request::cloud(prompt).with_timeout(cloud_timeout(cloud));
                let base = self.cloud_base_url();

                match cloud {
                    InferenceProvider::OpenAi => {
                        match base {
                            Some(base) => OpenAiProvider::with_base_url(base, &key, model)?,
                            None => OpenAiProvider::new(&key, model)?,
                        }
                        .complete(&request)
                        .await
                    }
                    InferenceProvider::Google => {
                        match base {
                            Some(base) => GoogleProvider::with_base_url(base, &key, model)?,
                            None => GoogleProvider::new(&key, model)?,
                        }
                        .complete(&request)
                        .await
                    }
                    _ => {
                        match base {
                            Some(base) => AnthropicProvider::with_base_url(base, &key, model)?,
                            None => AnthropicProvider::new(&key, model)?,
                        }
                        .complete(&request)
                        .await
                    }
                }
            }
            _ => {
                let model = config.inference.local_model_path.clone().ok_or_else(|| {
                    InferenceError::NotConfigured("no local model has been chosen".into())
                })?;

                let options = LlamaOptions {
                    binary: local_binary(&config.inference),
                    context_size: config.inference.context_size,
                    idle_unload: std::time::Duration::from_secs(
                        config.inference.idle_unload_seconds,
                    ),
                    ..LlamaOptions::for_model(model)
                };
                self.llama.complete(&options, &Request::local(prompt)).await
            }
        }
    }

    /// Where the cloud providers point. `None` in a real run, a fake server in a test.
    #[cfg(not(test))]
    fn cloud_base_url(&self) -> Option<String> {
        None
    }

    #[cfg(test)]
    fn cloud_base_url(&self) -> Option<String> {
        tests::base_url_override()
    }
}

/// Whether an hour's summary was written before the hour had finished happening.
///
/// A summary written at twenty past covers twenty minutes and then stands for the whole
/// hour, which is how a day summarized as it went ended up describing a fraction of it.
/// The stored `active_ms` is what the hour held when the summary was written, so more
/// activity since then means the summary is describing less than the hour now contains.
///
/// The minute of slack is there because the rollup moves by a few milliseconds as
/// episodes close, and rewriting an hour for that would spend a model call to produce
/// the same sentences.
fn is_stale(written: &HourSummary, hour: &oh_processing::rollup::HourlyRollup) -> bool {
    hour.active_ms.saturating_sub(written.active_ms) >= 60_000
}

/// Where `llama-server` is, if it can be found at all.
///
/// A path the user set wins over everything else, and a path that is no longer a file
/// is ignored rather than handed to the spawner, so a server that has been moved reads
/// as missing here instead of as a failure to start later. Failing that, the copy this
/// application fetched for itself (AD-30), and only then the search beside the
/// executable and on `PATH` for somebody who put one there by hand.
///
/// The fetched copy has to be checked here and not only where the settings page draws
/// its status. It was added to the status alone at first, which read as fetched and
/// ready in Settings while readiness went on saying it had not been fetched yet and
/// every summary went on failing — the download worked and nothing consumed it.
fn local_binary(inference: &oh_core::InferenceConfig) -> Option<std::path::PathBuf> {
    if let Some(chosen) = inference.local_server_path.as_ref() {
        return chosen.is_file().then(|| chosen.clone());
    }
    crate::runtime::installed().or_else(|| crate::llama::find_binary(None))
}

/// How long a cloud provider is given to answer.
fn cloud_timeout(provider: InferenceProvider) -> std::time::Duration {
    match provider {
        InferenceProvider::Google => GOOGLE_TIMEOUT,
        _ => CLOUD_TIMEOUT,
    }
}

/// How long to wait before the attempt after `attempt`.
///
/// Two seconds, then four. A provider that just rate-limited a burst needs a pause
/// rather than an immediate second ask, and a dropped connection does not care either
/// way, so the two are not worth telling apart. The whole ladder adds six seconds to a
/// run that is going to fail anyway, which is cheap against re-summarizing a day by
/// hand.
#[cfg(not(test))]
async fn backoff(attempt: u32) {
    tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
}

/// Tests assert what is retried, never how long the pause was. Sleeping here would buy
/// nothing and cost every test that exercises a failure.
#[cfg(test)]
async fn backoff(_attempt: u32) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHttp;
    use oh_core::InferenceConfig;
    use oh_processing::Episode;
    use oh_processing::rollup::{AppUsage, DailyRollup, HourlyRollup};
    use std::path::PathBuf;

    // Where the cloud provider points during a test. Thread-local rather than global:
    // the test harness runs tests in parallel, and a shared override would have one
    // test's server answering another test's request.
    thread_local! {
        static BASE_URL: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn base_url_override() -> Option<String> {
        BASE_URL.with(|slot| slot.borrow().clone())
    }

    /// Point the cloud provider at a fake server and give it a key to send. Both are
    /// thread-local, so nothing here touches the real Credential Manager.
    fn point_cloud_at(base: &str) {
        BASE_URL.with(|slot| *slot.borrow_mut() = Some(base.to_owned()));
        secrets::pretend_stored("sk-ant-test");
    }

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 22).unwrap()
    }

    fn episode(id: &str, app: &str, hour: u32, active_ms: i64) -> Episode {
        Episode {
            id: id.into(),
            date: "2026-08-22".into(),
            app: app.into(),
            app_path: None,
            title: Some(format!("{app} window")),
            titles: Vec::new(),
            urls: Vec::new(),
            documents: Vec::new(),
            visible_text: Vec::new(),
            start: format!("2026-08-22T{hour:02}:05:00.000Z"),
            end: format!("2026-08-22T{hour:02}:35:00.000Z"),
            duration_ms: 1_800_000,
            active_ms,
            event_count: 6,
            is_private: false,
        }
    }

    fn hourly(hour: u32, ids: &[&str], active_ms: i64) -> HourlyRollup {
        HourlyRollup {
            hour,
            active_ms,
            apps: vec![AppUsage {
                app: "Visual Studio Code".into(),
                active_ms,
                episodes: ids.len(),
            }],
            episode_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    /// Two hours of ordinary work.
    fn report() -> DayReport {
        let episodes = vec![
            episode("a", "Visual Studio Code", 9, 900_000),
            episode("b", "Google Chrome", 10, 900_000),
        ];
        DayReport {
            date: "2026-08-22".into(),
            rollup: DailyRollup {
                date: "2026-08-22".into(),
                active_ms: 1_800_000,
                idle_ms: 0,
                episodes: 2,
                apps: vec![AppUsage {
                    app: "Visual Studio Code".into(),
                    active_ms: 900_000,
                    episodes: 1,
                }],
                hours: vec![hourly(9, &["a"], 900_000), hourly(10, &["b"], 900_000)],
                first_activity: None,
                last_activity: None,
                private_episodes: 0,
            },
            episodes,
        }
    }

    fn cloud_config() -> Config {
        Config {
            inference: InferenceConfig {
                provider: InferenceProvider::Anthropic,
                cloud_consent: true,
                ..InferenceConfig::default()
            },
            ..Config::default()
        }
    }

    /// Settings that choose one model from the dropdown.
    fn using(model: &str) -> Config {
        let provider = oh_core::provider_for_model(model).expect("an offered model");
        Config {
            inference: InferenceConfig {
                provider,
                cloud_consent: true,
                cloud_model: model.to_owned(),
                ..InferenceConfig::default()
            },
            ..Config::default()
        }
    }

    fn answer(text: &str) -> String {
        format!(r#"{{"model":"claude-haiku-4-5","content":[{{"type":"text","text":"{text}"}}]}}"#)
    }

    #[test]
    fn a_disabled_provider_says_so_in_words_the_interface_can_show() {
        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();

        let readiness = service.readiness(&Config::default());
        assert!(!readiness.ready);
        assert_eq!(readiness.provider, "disabled");
        assert!(readiness.blocked_by.unwrap().contains("Settings"));
    }

    #[test]
    fn cloud_summaries_are_blocked_until_consent_is_given() {
        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();

        let mut config = cloud_config();
        config.inference.cloud_consent = false;

        let readiness = service.readiness(&config);
        assert!(!readiness.ready);
        assert!(readiness.blocked_by.unwrap().contains("agreement"));
    }

    #[test]
    fn a_local_model_that_is_not_on_disk_is_reported_as_missing() {
        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();

        let config = Config {
            inference: InferenceConfig {
                provider: InferenceProvider::Local,
                local_model_path: Some(PathBuf::from(r"C:\nowhere\model.gguf")),
                ..InferenceConfig::default()
            },
            ..Config::default()
        };

        let readiness = service.readiness(&config);
        assert!(!readiness.ready);
        assert!(readiness.blocked_by.unwrap().contains("missing"));
    }

    #[tokio::test]
    async fn a_run_without_consent_sends_nothing_and_writes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();

        let mut config = cloud_config();
        config.inference.cloud_consent = false;

        let error = service
            .summarize_day(&config, &report(), false)
            .await
            .unwrap_err();

        assert!(matches!(error, InferenceError::ConsentMissing));
        assert!(service.summary(date()).is_empty());
    }

    #[tokio::test]
    async fn a_full_run_writes_every_hour_and_then_the_day() {
        let server = FakeHttp::serving(200, &answer("Work happened.")).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();
        let run = service
            .summarize_day(&cloud_config(), &report(), false)
            .await
            .unwrap();

        assert_eq!(run.hours_written, vec![9, 10]);
        assert!(run.daily_written);
        assert_eq!(run.failure, None);

        let summary = service.summary(date());
        assert_eq!(summary.hours.len(), 2);
        assert_eq!(summary.hour(9).unwrap().text, "Work happened.");
        assert_eq!(summary.hour(9).unwrap().provider, "anthropic");
        assert_eq!(summary.daily.as_deref(), Some("Work happened."));

        // Two hours plus the day.
        assert_eq!(server.request_count(), 3);
    }

    #[tokio::test]
    async fn a_second_run_writes_nothing_new() {
        let server = FakeHttp::serving(200, &answer("Work happened.")).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();
        service
            .summarize_day(&cloud_config(), &report(), false)
            .await
            .unwrap();
        let before = server.request_count();

        let again = service
            .summarize_day(&cloud_config(), &report(), false)
            .await
            .unwrap();

        assert_eq!(again.hours_skipped, vec![9, 10]);
        assert!(again.hours_written.is_empty());
        assert!(!again.daily_written);
        assert_eq!(
            server.request_count(),
            before,
            "an unchanged day must not be re-summarized"
        );
    }

    #[tokio::test]
    async fn a_rewrite_replaces_everything() {
        let server = FakeHttp::serving(200, &answer("Work happened.")).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();
        service
            .summarize_day(&cloud_config(), &report(), false)
            .await
            .unwrap();

        let again = service
            .summarize_day(&cloud_config(), &report(), true)
            .await
            .unwrap();
        assert_eq!(again.hours_written, vec![9, 10]);
        assert!(again.daily_written);
    }

    #[test]
    fn google_is_given_longer_to_answer_than_the_other_clouds() {
        assert_eq!(cloud_timeout(InferenceProvider::Google), GOOGLE_TIMEOUT);
        assert_eq!(cloud_timeout(InferenceProvider::Anthropic), CLOUD_TIMEOUT);
        assert_eq!(cloud_timeout(InferenceProvider::OpenAi), CLOUD_TIMEOUT);
        assert!(
            GOOGLE_TIMEOUT > CLOUD_TIMEOUT,
            "the point of the constant is that it is longer"
        );
    }

    /// A blip is absorbed rather than reported. The server refuses twice and then
    /// answers, and the caller is told about a summary rather than a rate limit.
    #[tokio::test]
    async fn a_transient_failure_is_tried_again() {
        let server = FakeHttp::scripted(vec![
            (429, r#"{"error":{"message":"rate limited"}}"#),
            (503, r#"{"error":{"message":"overloaded"}}"#),
            (200, &answer("A focused hour.")),
        ])
        .await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();
        let written = service
            .summarize_hour(&cloud_config(), &report(), 9)
            .await
            .unwrap();

        assert_eq!(written.text, "A focused hour.");
        assert_eq!(
            server.request_count(),
            3,
            "both refusals should have been retried"
        );
    }

    /// Trying again is capped. The server never recovers, and the error the caller
    /// finally sees is the provider's own.
    #[tokio::test]
    async fn trying_again_gives_up_after_the_third_attempt() {
        let server = FakeHttp::serving(503, r#"{"error":{"message":"overloaded"}}"#).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();
        let error = service
            .summarize_hour(&cloud_config(), &report(), 9)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("overloaded"), "{error}");
        assert_eq!(server.request_count(), MAX_ATTEMPTS as usize);
    }

    /// A refusal that will be identical next time is reported at once. Asking again
    /// would spend the user's quota to arrive at the same answer.
    #[tokio::test]
    async fn a_failure_that_will_not_change_is_not_tried_again() {
        let server = FakeHttp::serving(400, r#"{"error":{"message":"malformed request"}}"#).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();
        let error = service
            .summarize_hour(&cloud_config(), &report(), 9)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("malformed request"), "{error}");
        assert_eq!(
            server.request_count(),
            1,
            "a 400 must be asked exactly once"
        );
    }

    /// The property that matters when a run fails half way: what was already written
    /// stays written.
    #[tokio::test]
    async fn a_failure_part_way_through_keeps_the_hours_already_written() {
        let server = FakeHttp::scripted(vec![
            (200, &answer("The morning went well.")),
            (429, r#"{"error":{"message":"rate limited"}}"#),
        ])
        .await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();
        let run = service
            .summarize_day(&cloud_config(), &report(), false)
            .await
            .unwrap();

        assert_eq!(run.hours_written, vec![9]);
        assert!(!run.daily_written);
        assert!(run.failure.unwrap().contains("rate limited"));

        let summary = service.summary(date());
        assert_eq!(summary.hours.len(), 1);
        assert_eq!(summary.hour(9).unwrap().text, "The morning went well.");
        assert_eq!(summary.daily, None);
    }

    #[tokio::test]
    async fn an_hour_can_be_rewritten_on_its_own() {
        let server = FakeHttp::serving(200, &answer("A focused hour.")).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();
        let written = service
            .summarize_hour(&cloud_config(), &report(), 9)
            .await
            .unwrap();

        assert_eq!(written.hour, 9);
        assert_eq!(written.text, "A focused hour.");
        assert_eq!(service.summary(date()).hours.len(), 1);
    }

    #[tokio::test]
    async fn an_hour_with_no_recorded_activity_is_refused_rather_than_invented() {
        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();

        let error = service
            .summarize_hour(&cloud_config(), &report(), 3)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("nothing was recorded"));
    }

    #[tokio::test]
    async fn an_empty_day_produces_no_requests_and_no_summary() {
        let server = FakeHttp::serving(200, &answer("Nothing.")).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();

        let empty = DayReport {
            date: "2026-08-22".into(),
            episodes: Vec::new(),
            rollup: DailyRollup {
                date: "2026-08-22".into(),
                active_ms: 0,
                idle_ms: 0,
                episodes: 0,
                apps: Vec::new(),
                hours: Vec::new(),
                first_activity: None,
                last_activity: None,
                private_episodes: 0,
            },
        };

        let run = service
            .summarize_day(&cloud_config(), &empty, false)
            .await
            .unwrap();

        assert!(!run.wrote_anything());
        assert_eq!(server.request_count(), 0);
        assert!(service.summary(date()).is_empty());
    }

    /// The dropdown chooses a model; the provider follows from it, and so does the
    /// endpoint the request lands on.
    #[tokio::test]
    async fn choosing_an_openai_model_sends_the_request_to_openai() {
        let body = r#"{"model":"gpt-5.6-luna","output":[{"type":"message","content":[{"type":"output_text","text":"A busy morning."}]}]}"#;
        let server = FakeHttp::serving(200, body).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();
        service
            .summarize_day(&using("gpt-5.6-luna"), &report(), false)
            .await
            .unwrap();

        let sent = server.last_request();
        assert!(sent.contains("POST /v1/responses"), "{sent}");
        assert_eq!(service.summary(date()).hour(9).unwrap().provider, "openai");
    }

    #[tokio::test]
    async fn choosing_the_gemini_model_sends_the_request_to_google() {
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"A busy morning."}]}}]}"#;
        let server = FakeHttp::serving(200, body).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();
        service
            .summarize_day(&using("gemini-flash-latest"), &report(), false)
            .await
            .unwrap();

        let sent = server.last_request();
        assert!(
            sent.contains("POST /v1beta/models/gemini-flash-latest:generateContent"),
            "{sent}"
        );
        assert_eq!(service.summary(date()).hour(9).unwrap().provider, "google");
    }

    #[tokio::test]
    async fn a_question_about_a_day_is_answered_from_that_day() {
        let server =
            FakeHttp::serving(200, &answer("You spent the morning in the collector.")).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();

        let reply = service
            .chat(&cloud_config(), &report(), &[], "What took the morning?")
            .await
            .unwrap();

        assert_eq!(reply.text, "You spent the morning in the collector.");
        let sent = server.last_request();
        assert!(sent.contains("What took the morning?"), "{sent}");
        assert!(sent.contains("Visual Studio Code"), "{sent}");
    }

    /// A conversation is not a document. Nothing about it is written beside the
    /// summaries, and asking a question does not disturb what is already there.
    #[tokio::test]
    async fn asking_a_question_writes_nothing_to_the_store() {
        let server = FakeHttp::serving(200, &answer("It went well.")).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();

        service
            .chat(&cloud_config(), &report(), &[], "What happened?")
            .await
            .unwrap();

        assert!(service.summary(date()).is_empty());
    }

    #[tokio::test]
    async fn the_conversation_so_far_goes_back_with_the_question() {
        let server = FakeHttp::serving(200, &answer("The afternoon.")).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();

        let turns = [ChatTurn {
            asked: "What took the morning?".into(),
            answered: "The collector.".into(),
        }];
        service
            .chat(&cloud_config(), &report(), &turns, "And after that?")
            .await
            .unwrap();

        let sent = server.last_request();
        assert!(sent.contains("The collector."), "{sent}");
        assert!(sent.contains("And after that?"), "{sent}");
    }

    /// The same gate as the summaries, because it is the same model and the same key.
    #[tokio::test]
    async fn nothing_is_asked_of_a_cloud_provider_without_consent() {
        let server = FakeHttp::serving(200, &answer("Never sent.")).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();

        let mut config = cloud_config();
        config.inference.cloud_consent = false;

        let error = service
            .chat(&config, &report(), &[], "What happened?")
            .await
            .unwrap_err();

        assert!(matches!(error, InferenceError::ConsentMissing), "{error}");
        assert_eq!(server.request_count(), 0);
    }

    #[tokio::test]
    async fn an_empty_question_is_refused_before_anything_is_sent() {
        let server = FakeHttp::serving(200, &answer("Never sent.")).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();

        let error = service
            .chat(&cloud_config(), &report(), &[], "   ")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("no question"), "{error}");
        assert_eq!(server.request_count(), 0);
    }

    #[test]
    fn each_cloud_provider_names_its_own_missing_key() {
        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();

        // Say outright that nothing is stored. Without this the real Credential
        // Manager answers, and the test passes only for someone who has never put a
        // key into the application.
        secrets::pretend_missing();

        for (model, expected) in [
            ("claude-haiku-4-5", "Anthropic API key"),
            ("gpt-5.6-terra", "OpenAI API key"),
            ("gemini-flash-latest", "Google AI Studio API key"),
        ] {
            let readiness = service.readiness(&using(model));
            assert!(!readiness.ready, "{model}");
            assert!(
                readiness.blocked_by.as_deref().unwrap().contains(expected),
                "{model}: {readiness:?}"
            );
            assert_eq!(readiness.model.as_deref(), Some(model));
        }
    }

    #[tokio::test]
    async fn no_cloud_provider_sends_anything_without_consent() {
        for model in ["claude-haiku-4-5", "gpt-5.6-sol", "gemini-flash-latest"] {
            let server = FakeHttp::serving(200, &answer("Nothing.")).await;
            point_cloud_at(&server.base_url());

            let temp = tempfile::tempdir().unwrap();
            let service = InferenceService::in_dir(temp.path()).unwrap();

            let mut config = using(model);
            config.inference.cloud_consent = false;

            let error = service
                .summarize_day(&config, &report(), false)
                .await
                .unwrap_err();
            assert!(matches!(error, InferenceError::ConsentMissing), "{model}");
            assert_eq!(server.request_count(), 0, "{model}");
        }
    }

    #[tokio::test]
    async fn forgetting_a_day_removes_its_summary() {
        let server = FakeHttp::serving(200, &answer("Work happened.")).await;
        point_cloud_at(&server.base_url());

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();
        service
            .summarize_day(&cloud_config(), &report(), false)
            .await
            .unwrap();
        assert!(!service.summary(date()).is_empty());

        service.forget(date()).unwrap();
        assert!(service.summary(date()).is_empty());
    }

    /// The prompt that goes to a provider must carry no private detail, and the only
    /// way to be sure is to look at what actually went over the socket.
    #[tokio::test]
    async fn nothing_private_reaches_the_provider() {
        let server = FakeHttp::serving(200, &answer("Some browsing.")).await;
        point_cloud_at(&server.base_url());

        let mut private = episode("p", "Google Chrome", 9, 900_000);
        private.is_private = true;
        private.title = Some("A page nobody should see".into());
        private.urls = vec!["https://example.com/secret?token=abc".into()];
        private.app_path = Some(r"C:\Users\someone\chrome.exe".into());

        let report = DayReport {
            date: "2026-08-22".into(),
            rollup: DailyRollup {
                date: "2026-08-22".into(),
                active_ms: 900_000,
                idle_ms: 0,
                episodes: 1,
                apps: Vec::new(),
                hours: vec![hourly(9, &["p"], 900_000)],
                first_activity: None,
                last_activity: None,
                private_episodes: 1,
            },
            episodes: vec![private],
        };

        let temp = tempfile::tempdir().unwrap();
        let service = InferenceService::in_dir(temp.path()).unwrap();
        service
            .summarize_day(&cloud_config(), &report, false)
            .await
            .unwrap();

        let sent = server.last_request();
        assert!(!sent.contains("nobody should see"), "{sent}");
        assert!(!sent.contains("token=abc"), "{sent}");
        assert!(!sent.contains("chrome.exe"), "{sent}");
        assert!(sent.contains("private session"), "{sent}");
    }
}
