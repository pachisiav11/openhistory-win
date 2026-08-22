//! Fetching a GGUF file, with progress and the ability to resume.
//!
//! These files run to several gigabytes, which makes two things non-negotiable. The
//! download reports progress, because a silent five-minute wait reads as a hang. And
//! it resumes, because a connection that drops at 90% must not cost the whole file.
//!
//! The partial file is `<name>.part` and is only renamed into place once the whole
//! body has arrived. Nothing ever sees a truncated `.gguf`, so
//! [`crate::catalog::statuses`] can treat "the file exists" as "the model is
//! installed".

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::catalog::CatalogModel;

/// How often a progress update is worth sending. Every chunk would be thousands of
/// events a second for a file this size.
const PROGRESS_INTERVAL_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("no model is known by the identifier {0}")]
    UnknownModel(String),
    #[error("the download was cancelled")]
    Cancelled,
    #[error("the server answered {status} for {url}")]
    Rejected { status: u16, url: String },
    #[error("{0}")]
    Transport(String),
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, DownloadError>;

/// How far along a download is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub downloaded_bytes: u64,
    /// The total, when the server declared one. A server that does not send a length
    /// leaves this `None` and the interface shows bytes rather than a percentage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    pub done: bool,
}

impl Progress {
    /// Completion as a fraction, when the total is known.
    pub fn fraction(&self) -> Option<f64> {
        let total = self.total_bytes.filter(|total| *total > 0)?;
        Some((self.downloaded_bytes as f64 / total as f64).min(1.0))
    }
}

/// Called as the download advances. Called from the download's own task.
pub type ProgressListener = Arc<dyn Fn(Progress) + Send + Sync>;

/// Set from another thread to abandon a download in progress.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Cancel::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Fetch a catalog model into `models_dir`, returning where it landed.
pub async fn fetch_model(
    model: &CatalogModel,
    models_dir: &Path,
    listener: Option<ProgressListener>,
    cancel: &Cancel,
) -> Result<PathBuf> {
    let target = model.path_in(models_dir);
    fetch_url(&model.download_url(), &target, listener, cancel).await?;
    Ok(target)
}

/// Fetch a URL to a path, resuming a `.part` file if one is there.
pub async fn fetch_url(
    url: &str,
    target: &Path,
    listener: Option<ProgressListener>,
    cancel: &Cancel,
) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|source| DownloadError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let partial = partial_path(target);
    let already = std::fs::metadata(&partial)
        .map(|meta| meta.len())
        .unwrap_or(0);

    let client = reqwest::Client::builder()
        .user_agent(concat!("openhistory-win/", env!("CARGO_PKG_VERSION")))
        // No overall timeout: a multi-gigabyte download legitimately takes a long
        // time. A stalled connection is caught by the read timeout instead.
        .read_timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|error| DownloadError::Transport(error.to_string()))?;

    let mut attempt = client.get(url);
    if already > 0 {
        attempt = attempt.header("range", format!("bytes={already}-"));
    }

    let response = attempt
        .send()
        .await
        .map_err(|error| DownloadError::Transport(error.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        return Err(DownloadError::Rejected {
            status: status.as_u16(),
            url: url.to_owned(),
        });
    }

    // 206 means the range was honoured and the body continues from `already`. Any
    // other success means the server sent the file from the start, so the partial
    // file must be discarded rather than appended to.
    let resuming = already > 0 && status.as_u16() == 206;
    let mut downloaded = if resuming { already } else { 0 };
    let total = response
        .content_length()
        .map(|length| length + if resuming { already } else { 0 });

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resuming)
        .truncate(!resuming)
        .open(&partial)
        .await
        .map_err(|source| DownloadError::Write {
            path: partial.clone(),
            source,
        })?;

    let mut reported = downloaded;
    let mut body = response.bytes_stream();

    while let Some(chunk) = body.next().await {
        if cancel.is_cancelled() {
            // The partial file is left where it is. A cancelled download that can be
            // resumed is more useful than a tidy directory.
            let _ = file.flush().await;
            return Err(DownloadError::Cancelled);
        }

        let chunk = chunk.map_err(|error| DownloadError::Transport(error.to_string()))?;
        file.write_all(&chunk)
            .await
            .map_err(|source| DownloadError::Write {
                path: partial.clone(),
                source,
            })?;

        downloaded += chunk.len() as u64;
        if downloaded - reported >= PROGRESS_INTERVAL_BYTES
            && let Some(listener) = listener.as_ref()
        {
            reported = downloaded;
            listener(Progress {
                downloaded_bytes: downloaded,
                total_bytes: total,
                done: false,
            });
        }
    }

    file.flush().await.map_err(|source| DownloadError::Write {
        path: partial.clone(),
        source,
    })?;
    drop(file);

    std::fs::rename(&partial, target).map_err(|source| DownloadError::Write {
        path: target.to_path_buf(),
        source,
    })?;

    if let Some(listener) = listener.as_ref() {
        listener(Progress {
            downloaded_bytes: downloaded,
            total_bytes: total.or(Some(downloaded)),
            done: true,
        });
    }
    Ok(())
}

/// Where a half-finished download lives.
pub fn partial_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    target.with_file_name(name)
}

/// Discard a half-finished download.
pub fn discard_partial(target: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(partial_path(target)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    /// A server that serves one fixed body and honours `Range`.
    ///
    /// Separate from `testing::FakeHttp` because this one has to speak ranges and
    /// serve arbitrary bytes rather than a JSON string, and mixing the two would make
    /// both harder to read.
    struct FileServer {
        port: u16,
        ranges: Arc<Mutex<Vec<Option<String>>>>,
        _shutdown: tokio::sync::oneshot::Sender<()>,
    }

    impl FileServer {
        async fn serving(body: Vec<u8>, honour_ranges: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let ranges: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
            let (shutdown, mut stop) = tokio::sync::oneshot::channel();

            let seen = Arc::clone(&ranges);
            tokio::spawn(async move {
                loop {
                    let accepted = tokio::select! {
                        accepted = listener.accept() => accepted,
                        _ = &mut stop => break,
                    };
                    let Ok((mut socket, _)) = accepted else { break };

                    let body = body.clone();
                    let seen = Arc::clone(&seen);
                    tokio::spawn(async move {
                        let mut buffer = [0u8; 2048];
                        let Ok(read) = socket.read(&mut buffer).await else {
                            return;
                        };
                        let request = String::from_utf8_lossy(&buffer[..read]).into_owned();

                        let range = request.lines().find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("range")
                                .then(|| value.trim().to_owned())
                        });
                        seen.lock().push(range.clone());

                        let start = range
                            .as_deref()
                            .filter(|_| honour_ranges)
                            .and_then(|value| value.strip_prefix("bytes="))
                            .and_then(|value| value.split('-').next())
                            .and_then(|value| value.parse::<usize>().ok())
                            .unwrap_or(0)
                            .min(body.len());

                        let slice = &body[start..];
                        let (status, reason) = if start > 0 {
                            (206, "Partial Content")
                        } else {
                            (200, "OK")
                        };

                        let head = format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                            slice.len()
                        );
                        let _ = socket.write_all(head.as_bytes()).await;
                        let _ = socket.write_all(slice).await;
                        let _ = socket.flush().await;
                    });
                }
            });

            FileServer {
                port,
                ranges,
                _shutdown: shutdown,
            }
        }

        fn url(&self) -> String {
            format!("http://127.0.0.1:{}/model.gguf", self.port)
        }

        fn ranges_seen(&self) -> Vec<Option<String>> {
            self.ranges.lock().clone()
        }
    }

    fn body(size: usize) -> Vec<u8> {
        (0..size).map(|n| (n % 251) as u8).collect()
    }

    #[tokio::test]
    async fn a_download_writes_the_whole_file_and_leaves_no_partial_behind() {
        let content = body(9_000_000);
        let server = FileServer::serving(content.clone(), true).await;
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("nested").join("model.gguf");

        let seen: Arc<Mutex<Vec<Progress>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let listener: ProgressListener = Arc::new(move |progress| recorder.lock().push(progress));

        fetch_url(&server.url(), &target, Some(listener), &Cancel::new())
            .await
            .unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), content);
        assert!(!partial_path(&target).exists());

        let progress = seen.lock().clone();
        assert!(
            progress.len() >= 2,
            "progress was not reported: {progress:?}"
        );
        let last = progress.last().unwrap();
        assert!(last.done);
        assert_eq!(last.downloaded_bytes, content.len() as u64);
        assert_eq!(last.fraction(), Some(1.0));
    }

    #[tokio::test]
    async fn an_interrupted_download_resumes_from_where_it_stopped() {
        let content = body(200_000);
        let server = FileServer::serving(content.clone(), true).await;
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("model.gguf");

        // Stand in for a previous attempt that got the first half.
        std::fs::write(partial_path(&target), &content[..80_000]).unwrap();

        fetch_url(&server.url(), &target, None, &Cancel::new())
            .await
            .unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), content);
        assert_eq!(
            server.ranges_seen(),
            vec![Some("bytes=80000-".to_string())],
            "the request did not ask to resume"
        );
    }

    /// A server that ignores `Range` sends the file from the start. Appending that to
    /// the partial file would produce a corrupt model that still looks complete.
    #[tokio::test]
    async fn a_server_that_ignores_the_range_restarts_the_file_rather_than_corrupting_it() {
        let content = body(120_000);
        let server = FileServer::serving(content.clone(), false).await;
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("model.gguf");
        std::fs::write(partial_path(&target), &content[..50_000]).unwrap();

        fetch_url(&server.url(), &target, None, &Cancel::new())
            .await
            .unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), content);
    }

    #[tokio::test]
    async fn a_cancelled_download_keeps_the_partial_file_and_writes_no_model() {
        let content = body(12_000_000);
        let server = FileServer::serving(content, true).await;
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("model.gguf");

        let cancel = Cancel::new();
        let trigger = cancel.clone();
        let listener: ProgressListener = Arc::new(move |_| trigger.cancel());

        let error = fetch_url(&server.url(), &target, Some(listener), &cancel)
            .await
            .unwrap_err();

        assert!(matches!(error, DownloadError::Cancelled));
        assert!(
            !target.exists(),
            "a cancelled download must not look installed"
        );
        assert!(partial_path(&target).exists(), "the partial must survive");
    }

    #[tokio::test]
    async fn a_missing_file_reports_the_status_rather_than_writing_an_error_page() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("model.gguf");
        // Nothing is listening on a port that was just released.
        let port = {
            let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            held.local_addr().unwrap().port()
        };

        let error = fetch_url(
            &format!("http://127.0.0.1:{port}/model.gguf"),
            &target,
            None,
            &Cancel::new(),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, DownloadError::Transport(_)));
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn a_catalog_model_lands_at_the_path_the_catalog_predicts() {
        let content = body(4096);
        let server = FileServer::serving(content.clone(), true).await;
        let temp = tempfile::tempdir().unwrap();

        let mut model = crate::catalog::find("qwen3-1.7b").unwrap();
        // Point the entry at the test server rather than reaching Hugging Face.
        let target = model.path_in(temp.path());
        model.repo = "test/repo".into();

        fetch_url(&server.url(), &target, None, &Cancel::new())
            .await
            .unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), content);
        let status = crate::catalog::statuses_in(temp.path(), None)
            .into_iter()
            .find(|status| status.model.id == "qwen3-1.7b")
            .unwrap();
        assert!(status.installed);
    }

    #[test]
    fn the_partial_file_sits_beside_the_target() {
        let target = Path::new(r"C:\models\gemma\gemma.gguf");
        assert_eq!(
            partial_path(target),
            PathBuf::from(r"C:\models\gemma\gemma.gguf.part")
        );
    }

    #[test]
    fn discarding_a_partial_that_is_not_there_is_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        discard_partial(&temp.path().join("model.gguf")).unwrap();
    }

    #[test]
    fn progress_with_no_declared_total_has_no_fraction() {
        let progress = Progress {
            downloaded_bytes: 100,
            total_bytes: None,
            done: false,
        };
        assert_eq!(progress.fraction(), None);
    }
}
