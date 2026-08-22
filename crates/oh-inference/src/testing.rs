//! A one-file HTTP server for the provider tests.
//!
//! Both providers speak HTTP, and both need to be verified against every answer a
//! server can give: a good one, an error, a malformed body, a hang. A real HTTP
//! framework would be a dependency added purely for the test build; the subset needed
//! here is small enough to write out, and writing it out means the tests assert
//! against the exact bytes that went over the socket.
//!
//! Compiled only under `cfg(test)`, so none of it reaches a release build.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// What the fake server does with a request.
#[derive(Clone)]
pub enum Behaviour {
    /// Answer with this status and body.
    Reply { status: u16, body: String },
    /// Answer with each body in turn, repeating the last one once exhausted. Used to
    /// script a server that is unhealthy and then becomes healthy.
    Sequence(Arc<Mutex<Vec<(u16, String)>>>),
    /// Accept the request and never answer.
    Hang,
}

pub struct FakeHttp {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
    /// Dropping this shuts the accept loop down, so a test's server does not outlive it.
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

impl FakeHttp {
    /// A server that answers every request the same way.
    pub async fn serving(status: u16, body: &str) -> Self {
        Self::with_behaviour(Behaviour::Reply {
            status,
            body: body.to_owned(),
        })
        .await
    }

    /// A server that never answers, for testing timeouts.
    pub async fn hanging() -> Self {
        Self::with_behaviour(Behaviour::Hang).await
    }

    /// A server that answers with each scripted response in turn.
    pub async fn scripted(responses: Vec<(u16, &str)>) -> Self {
        let scripted = responses
            .into_iter()
            .map(|(status, body)| (status, body.to_owned()))
            .collect();
        Self::with_behaviour(Behaviour::Sequence(Arc::new(Mutex::new(scripted)))).await
    }

    pub async fn with_behaviour(behaviour: Behaviour) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let (shutdown, mut stop) = tokio::sync::oneshot::channel();

        let recorder = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    accepted = listener.accept() => accepted,
                    _ = &mut stop => break,
                };
                let Ok((mut socket, _)) = accepted else { break };

                let behaviour = behaviour.clone();
                let recorder = Arc::clone(&recorder);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 4096];

                    // Read until the body has arrived. Every request these tests make
                    // declares a Content-Length, and one that does not is a GET with
                    // no body, which ends at the blank line.
                    loop {
                        let Ok(read) = socket.read(&mut buffer).await else {
                            return;
                        };
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&buffer[..read]);
                        if is_complete(&request) {
                            break;
                        }
                    }

                    recorder
                        .lock()
                        .push(String::from_utf8_lossy(&request).into_owned());

                    let (status, body) = match &behaviour {
                        Behaviour::Hang => {
                            tokio::time::sleep(Duration::from_secs(3600)).await;
                            return;
                        }
                        Behaviour::Reply { status, body } => (*status, body.clone()),
                        Behaviour::Sequence(remaining) => {
                            let mut remaining = remaining.lock();
                            if remaining.len() > 1 {
                                remaining.remove(0)
                            } else {
                                remaining
                                    .first()
                                    .cloned()
                                    .unwrap_or((500, "exhausted".to_owned()))
                            }
                        }
                    };

                    let response = format!(
                        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        reason(status),
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });

        FakeHttp {
            port,
            seen,
            _shutdown: shutdown,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The most recent request, headers and body, exactly as it arrived.
    pub fn last_request(&self) -> String {
        self.seen.lock().last().cloned().unwrap_or_default()
    }

    pub fn request_count(&self) -> usize {
        self.seen.lock().len()
    }
}

/// True once the whole request has arrived.
fn is_complete(request: &[u8]) -> bool {
    let text = String::from_utf8_lossy(request);
    let Some(header_end) = text.find("\r\n\r\n") else {
        return false;
    };

    let declared = text[..header_end]
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);

    request.len() >= header_end + 4 + declared
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}
