//! The local model, run by a `llama-server` this application starts and stops.
//!
//! Per AD-3 the model is not held resident. A summarization run starts the server,
//! waits for `/health`, generates, and leaves the server up for an idle period so that
//! a run of hourly summaries reuses one load. A watchdog shuts it down after that.
//!
//! Three failure modes shaped this file:
//!
//! - The binary may not be installed. That is an ordinary state, not a crash: the
//!   interface says the local provider is unavailable and why.
//! - The port may already be in use, by another program or by a `llama-server` this
//!   application left behind when it crashed. A server already answering `/health`
//!   with the right model is adopted rather than fought with.
//! - Loading a model takes seconds and occasionally minutes. Readiness is polled, and
//!   never assumed after a sleep.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;
use tokio::process::{Child, Command};

use crate::provider::{Completion, InferenceError, Request, Result, tidy};

pub const PROVIDER: &str = "local";

/// The executable, looked up on `PATH` when no explicit path is configured.
pub const BINARY_NAME: &str = "llama-server";

/// How long to wait for a model to load before giving up.
const READY_TIMEOUT: Duration = Duration::from_secs(180);

/// How often to ask `/health` while waiting.
const HEALTH_POLL: Duration = Duration::from_millis(400);

/// How long to wait for `/health` on a single attempt. A loading server answers 503
/// promptly; one that never answers at all is not the one we want.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);

/// How the server should be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlamaOptions {
    /// Path to `llama-server`. When `None`, the name is used and `PATH` resolves it.
    pub binary: Option<PathBuf>,
    pub model: PathBuf,
    pub context_size: u32,
    /// How long the server may sit unused before the watchdog stops it.
    pub idle_unload: Duration,
    /// Preferred port. When `None`, a free one is chosen.
    pub port: Option<u16>,
    /// Extra arguments, passed through verbatim. This is the escape hatch for
    /// `-ngl`, `--threads`, and anything else a particular machine needs.
    pub extra_args: Vec<String>,
}

impl LlamaOptions {
    pub fn for_model(model: impl Into<PathBuf>) -> Self {
        LlamaOptions {
            binary: None,
            model: model.into(),
            context_size: 8192,
            idle_unload: Duration::from_secs(300),
            port: None,
            extra_args: Vec::new(),
        }
    }

    /// The model's file stem, which is what gets recorded as the model that wrote a
    /// summary. The full path is a machine detail and is not stored in a summary.
    pub fn model_name(&self) -> String {
        self.model
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "local model".to_owned())
    }

    fn binary_path(&self) -> PathBuf {
        self.binary
            .clone()
            .unwrap_or_else(|| PathBuf::from(BINARY_NAME))
    }
}

/// What the interface shows about the local server.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlamaStatus {
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// True when this application started the process, false when it adopted one that
    /// was already there.
    pub managed: bool,
    /// Seconds since the last generation, when running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_seconds: Option<u64>,
}

impl LlamaStatus {
    pub fn stopped() -> Self {
        LlamaStatus {
            running: false,
            port: None,
            model: None,
            managed: false,
            idle_seconds: None,
        }
    }
}

struct Running {
    port: u16,
    model: String,
    /// `None` when the process was already there and was adopted; killing something
    /// this application did not start would be rude and possibly destructive.
    child: Option<Child>,
    /// The tail of what the server wrote to stderr, kept so a failure can be reported
    /// in the server's own words rather than guessed at. Empty for an adopted process.
    said: Arc<Mutex<Vec<String>>>,
    last_used: Instant,
}

/// How many lines of the server's output to keep for a failure report.
///
/// llama.cpp writes its whole startup log to stderr, so the interesting line — the one
/// naming the argument it would not take or the tensor it could not read — is at the
/// end, after a screen of banners.
const KEPT_LINES: usize = 20;

/// A `llama-server` process and the client that talks to it.
pub struct LlamaServer {
    client: reqwest::Client,
    running: Arc<Mutex<Option<Running>>>,
    watchdog: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Default for LlamaServer {
    fn default() -> Self {
        Self::new()
    }
}

impl LlamaServer {
    pub fn new() -> Self {
        LlamaServer {
            client: reqwest::Client::builder()
                .build()
                .expect("a client with no configuration always builds"),
            running: Arc::new(Mutex::new(None)),
            watchdog: Mutex::new(None),
        }
    }

    pub fn status(&self) -> LlamaStatus {
        match self.running.lock().as_ref() {
            None => LlamaStatus::stopped(),
            Some(running) => LlamaStatus {
                running: true,
                port: Some(running.port),
                model: Some(running.model.clone()),
                managed: running.child.is_some(),
                idle_seconds: Some(running.last_used.elapsed().as_secs()),
            },
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().is_some()
    }

    /// Start the server if it is not already up, and wait until it answers.
    ///
    /// Returns the port it is listening on.
    pub async fn ensure_started(&self, options: &LlamaOptions) -> Result<u16> {
        if let Some(running) = self.running.lock().as_ref() {
            return Ok(running.port);
        }

        if !options.model.is_file() {
            return Err(InferenceError::NotConfigured(format!(
                "the model file {} is not there",
                options.model.display()
            )));
        }

        // A server left behind by a previous run, or started by hand, is adopted
        // rather than duplicated. Two of them on one machine would load the model
        // twice, which is exactly the memory cost this design exists to avoid.
        if let Some(port) = options.port
            && self.is_healthy(port).await
        {
            tracing::info!(port, "adopting a llama-server that was already listening");
            // Nothing to collect: this process is somebody else's, and its output went
            // wherever they sent it.
            self.adopt(
                port,
                options.model_name(),
                None,
                Arc::new(Mutex::new(Vec::new())),
            );
            self.start_watchdog(options.idle_unload);
            return Ok(port);
        }

        let port = match options.port {
            Some(preferred) if is_port_free(preferred) => preferred,
            Some(taken) => {
                let chosen = free_port()?;
                tracing::warn!(taken, chosen, "the preferred llama-server port was in use");
                chosen
            }
            None => free_port()?,
        };

        let binary = options.binary_path();
        let mut command = Command::new(&binary);
        command
            .arg("--model")
            .arg(&options.model)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--ctx-size")
            .arg(options.context_size.to_string())
            // No CORS flag is passed. It used to send `--cors-allow-origin *`, which
            // llama.cpp renamed to `--cors-origins`: build b10612 refuses the old
            // spelling outright, and since every request to this server is made from
            // the Rust side rather than from the window, the browser's origin rules
            // were never in play to begin with. The current default is `*` regardless.
            .args(&options.extra_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // Kept rather than discarded so that a server which dies during startup can
            // be reported in its own words. This has to be drained, or a server that
            // logs more than the pipe holds would block forever on a full buffer.
            .stderr(Stdio::piped())
            // A release build has no console, so the server's own window would flash
            // up behind the application otherwise.
            .kill_on_drop(true);

        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command.spawn().map_err(|error| {
            InferenceError::ServerUnavailable(if error.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "{} was not found. Install llama.cpp and put llama-server on PATH, or set its \
                     path in settings.",
                    binary.display()
                )
            } else {
                format!("{} could not be started: {error}", binary.display())
            })
        })?;

        let said = Arc::new(Mutex::new(Vec::new()));
        if let Some(stream) = child.stderr.take() {
            let collected = Arc::clone(&said);
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt, BufReader};
                let mut lines = BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut held = collected.lock();
                    if held.len() == KEPT_LINES {
                        held.remove(0);
                    }
                    held.push(line);
                }
            });
        }

        self.adopt(port, options.model_name(), Some(child), said);

        if let Err(error) = self.wait_until_ready(port).await {
            self.stop().await;
            return Err(error);
        }

        self.start_watchdog(options.idle_unload);
        Ok(port)
    }

    /// Generate one summary, starting the server first if it is not up.
    pub async fn complete(&self, options: &LlamaOptions, request: &Request) -> Result<Completion> {
        let port = self.ensure_started(options).await?;
        let model = options.model_name();

        let body = json!({
            "model": "local",
            "messages": [
                { "role": "system", "content": request.prompt.system },
                { "role": "user", "content": request.prompt.user },
            ],
            "max_tokens": request.prompt.max_tokens,
            "temperature": 0.3,
            "stream": false,
            // A reasoning model spends its output budget thinking before it writes
            // anything, and a summary is not a problem that needs working through: the
            // hourly budget went entirely on a chain of thought that restated the
            // instructions, leaving `content` empty and the run reported as "local
            // returned an empty summary". Asked not to think, the same model answers
            // well inside the same budget.
            //
            // Sent in the body rather than as a spawn argument on purpose. A server
            // that does not know this field ignores it, whereas an argument a build
            // does not recognise is fatal at startup — which is exactly how
            // `--cors-allow-origin` broke local inference (AD-32).
            "chat_template_kwargs": { "enable_thinking": false },
        });

        let response = self
            .client
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .header("content-type", "application/json")
            .timeout(request.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    InferenceError::TimedOut {
                        provider: PROVIDER,
                        seconds: request.timeout.as_secs(),
                    }
                } else {
                    InferenceError::Transport(error.to_string())
                }
            })?;

        self.touch();

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| InferenceError::Transport(error.to_string()))?;

        if !status.is_success() {
            return Err(InferenceError::Rejected {
                provider: PROVIDER,
                status: status.as_u16(),
                message: text.trim().chars().take(400).collect(),
            });
        }

        let parsed: ChatResponse = serde_json::from_str(&text).map_err(|error| {
            InferenceError::Transport(format!("could not read the llama-server response: {error}"))
        })?;

        let choice = parsed.choices.first();
        let cleaned = choice
            .map(|choice| tidy(&choice.message.content))
            .unwrap_or_default();

        if cleaned.is_empty() {
            // The same shape the OpenAI provider already reports: an answer that never
            // began because the thinking used the whole budget comes back as a success
            // with nothing in it, and "the model said nothing" gives no one anything to
            // act on.
            let thought_instead = choice.is_some_and(|choice| {
                choice.finish_reason.as_deref() == Some("length")
                    || choice
                        .message
                        .reasoning_content
                        .as_deref()
                        .is_some_and(|thinking| !thinking.trim().is_empty())
            });
            if thought_instead {
                return Err(InferenceError::Rejected {
                    provider: PROVIDER,
                    status: status.as_u16(),
                    message: "the model used its whole output budget thinking and wrote no \
                              summary. This request asks for thinking to be switched off, so \
                              this model's template is not honouring that; a different local \
                              model will do better."
                        .to_owned(),
                });
            }
            return Err(InferenceError::Empty { provider: PROVIDER });
        }

        Ok(Completion {
            text: cleaned,
            provider: PROVIDER,
            model,
        })
    }

    /// Stop the server, if this application started it.
    pub async fn stop(&self) {
        if let Some(handle) = self.watchdog.lock().take() {
            handle.abort();
        }

        let child = self.running.lock().take().and_then(|running| running.child);
        if let Some(mut child) = child {
            let _ = child.kill().await;
            let _ = child.wait().await;
            tracing::info!("llama-server stopped");
        }
    }

    /// Note that the server was just used, so the watchdog starts counting again.
    fn touch(&self) {
        if let Some(running) = self.running.lock().as_mut() {
            running.last_used = Instant::now();
        }
    }

    fn adopt(&self, port: u16, model: String, child: Option<Child>, said: Arc<Mutex<Vec<String>>>) {
        *self.running.lock() = Some(Running {
            port,
            model,
            child,
            said,
            last_used: Instant::now(),
        });
    }

    /// The tail of what the server wrote before it stopped.
    fn what_it_said(&self) -> Vec<String> {
        match self.running.lock().as_ref() {
            Some(running) => running.said.lock().clone(),
            None => Vec::new(),
        }
    }

    async fn is_healthy(&self, port: u16) -> bool {
        matches!(self.health(port).await, Health::Ready)
    }

    async fn health(&self, port: u16) -> Health {
        let sent = self
            .client
            .get(format!("http://127.0.0.1:{port}/health"))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .await;

        match sent {
            Ok(response) if response.status().is_success() => Health::Ready,
            // 503 is what llama-server answers while the model is still loading.
            Ok(_) => Health::Loading,
            Err(_) => Health::Unreachable,
        }
    }

    /// Poll `/health` until the model has loaded.
    async fn wait_until_ready(&self, port: u16) -> Result<()> {
        let deadline = Instant::now() + READY_TIMEOUT;

        loop {
            // A server that has exited will never become healthy, however long the
            // deadline is. Notice it immediately rather than waiting three minutes.
            if self.has_exited() {
                return Err(InferenceError::ServerUnavailable(explain_exit(
                    &self.what_it_said(),
                )));
            }

            match self.health(port).await {
                Health::Ready => return Ok(()),
                Health::Loading | Health::Unreachable => {}
            }

            if Instant::now() >= deadline {
                return Err(InferenceError::ServerUnavailable(format!(
                    "llama-server did not finish loading the model within {}s",
                    READY_TIMEOUT.as_secs()
                )));
            }
            tokio::time::sleep(HEALTH_POLL).await;
        }
    }

    fn has_exited(&self) -> bool {
        let mut guard = self.running.lock();
        let Some(running) = guard.as_mut() else {
            return true;
        };
        match running.child.as_mut() {
            Some(child) => !matches!(child.try_wait(), Ok(None)),
            // An adopted process is not ours to watch.
            None => false,
        }
    }

    /// Shut the server down once it has been unused for the idle period.
    fn start_watchdog(&self, idle_unload: Duration) {
        if idle_unload.is_zero() {
            return;
        }
        let mut slot = self.watchdog.lock();
        if slot.is_some() {
            return;
        }

        let running = Arc::clone(&self.running);
        *slot = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(idle_unload.min(Duration::from_secs(30))).await;

                let expired = {
                    let guard = running.lock();
                    match guard.as_ref() {
                        None => return,
                        Some(state) => state.last_used.elapsed() >= idle_unload,
                    }
                };
                if !expired {
                    continue;
                }

                let child = running.lock().take().and_then(|state| state.child);
                if let Some(mut child) = child {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    tracing::info!("llama-server unloaded after sitting idle");
                }
                return;
            }
        }));
    }
}

impl Drop for LlamaServer {
    fn drop(&mut self) {
        if let Some(handle) = self.watchdog.lock().take() {
            handle.abort();
        }
        // `kill_on_drop` on the command handles the process itself, which matters
        // because a `Drop` cannot await the wait.
    }
}

enum Health {
    Ready,
    Loading,
    Unreachable,
}

/// Why the server stopped, in its own words where it gave any.
///
/// This used to be a fixed sentence blaming the GGUF or the machine's memory. Both were
/// guesses, and for a real failure both were wrong: the server was rejecting an argument
/// this application had passed it (`--cors-allow-origin`, which llama.cpp had renamed),
/// and it said so plainly on a stderr that was being discarded. Somebody reading
/// "the file may not be a valid GGUF" re-downloads a three-gigabyte model that was never
/// the problem.
fn explain_exit(said: &[String]) -> String {
    // llama.cpp prefixes a fatal argument or load failure with "error:". That line is
    // the answer when it exists; the rest of the log is banners.
    let complaint = said
        .iter()
        .rev()
        .find(|line| line.to_lowercase().contains("error"))
        .or_else(|| said.last());

    match complaint {
        Some(line) => format!("llama-server stopped while starting up: {}", line.trim()),
        None => "llama-server stopped while starting up without saying why. The model file \
                 may be incomplete, or the machine may not have the memory for it."
            .to_owned(),
    }
}

/// A port nothing is listening on.
pub fn free_port() -> Result<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|error| {
            InferenceError::ServerUnavailable(format!("no local port could be claimed: {error}"))
        })
}

/// True when nothing is listening on this port right now.
pub fn is_port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Find `llama-server` next to the application, in the resources directory, or on
/// `PATH`.
///
/// The bundled copy wins: a user who installed the application should get the version
/// it was tested against, not whatever happens to be on `PATH`.
pub fn find_binary(resource_dir: Option<&Path>) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        "llama-server.exe"
    } else {
        BINARY_NAME
    };

    if let Some(resources) = resource_dir {
        for candidate in [resources.join(file), resources.join("resources").join(file)] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(file);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    which_on_path(file)
}

fn which_on_path(file: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(file))
        .find(|candidate| candidate.is_file())
}

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChatMessage,
    /// `"length"` when the answer stopped on the token ceiling rather than finishing.
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: String,
    /// Where a reasoning model puts its thinking.
    ///
    /// Never used as the summary — it is a working note addressed to itself, not an
    /// answer — but its presence is what explains an empty `content`.
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::Prompt;
    use crate::testing::FakeHttp;

    /// The real failure this replaces: a rejected argument reported as a bad model
    /// file, sending the user to re-download three gigabytes that were never at fault.
    #[test]
    fn a_server_that_refused_an_argument_is_quoted_rather_than_guessed_at() {
        let said = [
            "build: 10612 (758443071) with Clang 20.1.8".to_owned(),
            "error: invalid argument: --cors-allow-origin".to_owned(),
        ];
        let explained = explain_exit(&said);

        assert!(explained.contains("--cors-allow-origin"), "{explained}");
        assert!(!explained.contains("valid GGUF"), "{explained}");
    }

    #[test]
    fn a_server_that_said_nothing_admits_that_rather_than_inventing_a_cause() {
        let explained = explain_exit(&[]);
        assert!(explained.contains("without saying why"), "{explained}");
    }

    fn request() -> Request {
        Request::local(Prompt {
            system: "You summarize.".into(),
            user: "What happened?".into(),
            max_tokens: 300,
        })
    }

    fn model_file() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gemma-4-e2b-it-qat-q4_0.gguf");
        std::fs::write(&path, b"GGUF").unwrap();
        (temp, path)
    }

    #[test]
    fn a_model_reports_its_file_stem_as_its_name() {
        let options = LlamaOptions::for_model(r"C:\models\qwen\Qwen3.5-2B-Instruct-Q4_K_M.gguf");
        assert_eq!(options.model_name(), "Qwen3.5-2B-Instruct-Q4_K_M");
    }

    #[test]
    fn a_free_port_is_actually_free() {
        let port = free_port().unwrap();
        assert!(is_port_free(port));
    }

    #[test]
    fn a_bound_port_is_reported_as_taken() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        assert!(!is_port_free(port));
        drop(held);
    }

    #[tokio::test]
    async fn a_missing_model_file_is_refused_before_anything_is_spawned() {
        let server = LlamaServer::new();
        let options = LlamaOptions::for_model(r"C:\nowhere\missing.gguf");

        let error = server.ensure_started(&options).await.unwrap_err();
        assert!(matches!(error, InferenceError::NotConfigured(_)));
        assert!(error.to_string().contains("missing.gguf"));
        assert!(!server.is_running());
    }

    #[tokio::test]
    async fn a_missing_binary_says_how_to_install_it() {
        let (_temp, model) = model_file();
        let server = LlamaServer::new();
        let options = LlamaOptions {
            binary: Some(PathBuf::from(r"C:\nowhere\llama-server.exe")),
            ..LlamaOptions::for_model(model)
        };

        let error = server.ensure_started(&options).await.unwrap_err();
        assert!(matches!(error, InferenceError::ServerUnavailable(_)));
        assert!(error.to_string().contains("PATH"), "{error}");
        assert!(!server.is_running());
    }

    /// The whole point of adoption: a healthy server on the configured port is used as
    /// it is, and nothing is spawned. The fake server is standing in for a
    /// `llama-server` this application left behind.
    #[tokio::test]
    async fn a_server_already_listening_is_adopted_rather_than_duplicated() {
        let (_temp, model) = model_file();
        let fake = FakeHttp::serving(200, r#"{"status":"ok"}"#).await;

        let server = LlamaServer::new();
        let options = LlamaOptions {
            port: Some(fake.port()),
            // Deliberately unusable: adoption must happen before any spawn is tried.
            binary: Some(PathBuf::from(r"C:\nowhere\llama-server.exe")),
            idle_unload: Duration::ZERO,
            ..LlamaOptions::for_model(model)
        };

        assert_eq!(server.ensure_started(&options).await.unwrap(), fake.port());

        let status = server.status();
        assert!(status.running);
        assert!(!status.managed, "an adopted process is not ours to kill");
        assert_eq!(status.model.as_deref(), Some("gemma-4-e2b-it-qat-q4_0"));

        server.stop().await;
        assert!(!server.is_running());
    }

    #[tokio::test]
    async fn a_completion_is_parsed_from_the_openai_shaped_answer() {
        let (_temp, model) = model_file();
        let fake = FakeHttp::scripted(vec![
            (200, r#"{"status":"ok"}"#),
            (
                200,
                r#"{"choices":[{"message":{"role":"assistant","content":"You edited the collector."}}]}"#,
            ),
        ])
        .await;

        let server = LlamaServer::new();
        let options = LlamaOptions {
            port: Some(fake.port()),
            idle_unload: Duration::ZERO,
            ..LlamaOptions::for_model(model)
        };

        let completion = server.complete(&options, &request()).await.unwrap();
        assert_eq!(completion.text, "You edited the collector.");
        assert_eq!(completion.provider, "local");
        assert_eq!(completion.model, "gemma-4-e2b-it-qat-q4_0");

        let sent = fake.last_request();
        assert!(sent.contains("POST /v1/chat/completions"), "{sent}");
        assert!(sent.contains("\"stream\":false"), "{sent}");
        assert!(sent.contains("You summarize."), "{sent}");

        server.stop().await;
    }

    /// The shape a real reasoning model returned: the whole budget spent in
    /// `reasoning_content`, `content` empty, and the run reported to the user as
    /// "local returned an empty summary" — true, and no use to anybody.
    #[tokio::test]
    async fn a_model_that_only_thought_says_so_rather_than_looking_silent() {
        let (_temp, model) = model_file();
        let fake = FakeHttp::scripted(vec![
            (200, r#"{"status":"ok"}"#),
            (
                200,
                r#"{"choices":[{"finish_reason":"length","message":{"role":"assistant","content":"","reasoning_content":"Thinking Process: the user wants a summary..."}}]}"#,
            ),
        ])
        .await;

        let server = LlamaServer::new();
        let options = LlamaOptions {
            port: Some(fake.port()),
            idle_unload: Duration::ZERO,
            ..LlamaOptions::for_model(model)
        };

        let error = server.complete(&options, &request()).await.unwrap_err();
        match error {
            InferenceError::Rejected { message, .. } => {
                assert!(message.contains("thinking"), "{message}");
            }
            other => panic!("expected the thinking case to be named, got {other:?}"),
        }
        server.stop().await;
    }

    #[tokio::test]
    async fn a_server_that_answers_with_no_choices_is_an_error() {
        let (_temp, model) = model_file();
        let fake = FakeHttp::scripted(vec![
            (200, r#"{"status":"ok"}"#),
            (200, r#"{"choices":[]}"#),
        ])
        .await;

        let server = LlamaServer::new();
        let options = LlamaOptions {
            port: Some(fake.port()),
            idle_unload: Duration::ZERO,
            ..LlamaOptions::for_model(model)
        };

        assert!(matches!(
            server.complete(&options, &request()).await.unwrap_err(),
            InferenceError::Empty { .. }
        ));
        server.stop().await;
    }

    #[tokio::test]
    async fn a_server_error_is_reported_with_its_status() {
        let (_temp, model) = model_file();
        let fake = FakeHttp::scripted(vec![
            (200, r#"{"status":"ok"}"#),
            (500, r#"{"error":"context overflow"}"#),
        ])
        .await;

        let server = LlamaServer::new();
        let options = LlamaOptions {
            port: Some(fake.port()),
            idle_unload: Duration::ZERO,
            ..LlamaOptions::for_model(model)
        };

        match server.complete(&options, &request()).await.unwrap_err() {
            InferenceError::Rejected { status, .. } => assert_eq!(status, 500),
            other => panic!("expected a rejection, got {other:?}"),
        }
        server.stop().await;
    }

    #[tokio::test]
    async fn a_request_that_does_not_answer_in_time_times_out_rather_than_hanging() {
        let (_temp, model) = model_file();
        let fake = FakeHttp::hanging().await;

        let server = LlamaServer::new();
        // Adoption needs a healthy server, and this one never answers, so drive the
        // completion path directly by adopting it by hand.
        server.adopt(
            fake.port(),
            "test".to_owned(),
            None,
            Arc::new(Mutex::new(Vec::new())),
        );

        let options = LlamaOptions {
            port: Some(fake.port()),
            idle_unload: Duration::ZERO,
            ..LlamaOptions::for_model(model)
        };
        let mut quick = request();
        quick.timeout = Duration::from_millis(300);

        match server.complete(&options, &quick).await.unwrap_err() {
            InferenceError::TimedOut { provider, .. } => assert_eq!(provider, "local"),
            other => panic!("expected a timeout, got {other:?}"),
        }
        server.stop().await;
    }

    #[tokio::test]
    async fn stopping_a_server_that_was_never_started_is_not_an_error() {
        LlamaServer::new().stop().await;
    }

    #[test]
    fn a_stopped_server_reports_itself_as_stopped() {
        let status = LlamaServer::new().status();
        assert!(!status.running);
        assert_eq!(status.port, None);
    }

    #[test]
    fn a_bundled_binary_is_preferred_over_whatever_is_on_path() {
        let temp = tempfile::tempdir().unwrap();
        let file = if cfg!(windows) {
            "llama-server.exe"
        } else {
            "llama-server"
        };
        let bundled = temp.path().join(file);
        std::fs::write(&bundled, b"").unwrap();

        assert_eq!(find_binary(Some(temp.path())), Some(bundled));
    }

    #[test]
    fn a_resources_subdirectory_is_searched_too() {
        let temp = tempfile::tempdir().unwrap();
        let file = if cfg!(windows) {
            "llama-server.exe"
        } else {
            "llama-server"
        };
        let nested = temp.path().join("resources");
        std::fs::create_dir_all(&nested).unwrap();
        let bundled = nested.join(file);
        std::fs::write(&bundled, b"").unwrap();

        assert_eq!(find_binary(Some(temp.path())), Some(bundled));
    }
}
