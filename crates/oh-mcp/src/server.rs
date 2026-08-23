//! The HTTP surface: a REST view of the history and an MCP endpoint over JSON-RPC.
//!
//! Bound to `127.0.0.1` only. Nothing here listens on a routable address, and the
//! bearer token is checked on every route except `/mcp/v1/health`, which exists so a
//! client can tell whether the port belongs to this application before it sends a
//! credential to it.
//!
//! Two shapes over the same data. The REST routes are what a shell script or a `curl`
//! line wants; the JSON-RPC endpoint at `/mcp` is what an MCP client speaks. Both go
//! through [`crate::History`], so neither can return more than the other.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::NaiveDate;
use oh_core::McpConfig;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::history::History;
use crate::rpc;
use crate::tokens::{self, TokenStore};

/// Loopback only. A history server that answers the network is a different product.
const ADDRESS: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// The most search results one request may ask for.
const MAX_LIMIT: usize = 200;

/// What the server shares with every handler.
#[derive(Clone)]
pub struct ServerState {
    pub history: History,
    pub tokens: Arc<Mutex<TokenStore>>,
    /// Answer about days other than today.
    pub allow_history: bool,
}

/// What the settings page shows about the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The address to paste into a client's configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A token exists, so a client could authenticate.
    pub has_token: bool,
}

impl McpStatus {
    pub fn stopped(has_token: bool) -> Self {
        McpStatus {
            running: false,
            port: None,
            url: None,
            has_token,
        }
    }
}

struct Running {
    port: u16,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

/// The server, and whether it is up.
#[derive(Default)]
pub struct McpServer {
    running: Mutex<Option<Running>>,
}

impl McpServer {
    pub fn new() -> Self {
        McpServer::default()
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().is_some()
    }

    pub fn port(&self) -> Option<u16> {
        self.running.lock().as_ref().map(|running| running.port)
    }

    pub fn status(&self, has_token: bool) -> McpStatus {
        match self.port() {
            Some(port) => McpStatus {
                running: true,
                port: Some(port),
                url: Some(format!("http://{ADDRESS}:{port}")),
                has_token,
            },
            None => McpStatus::stopped(has_token),
        }
    }

    /// Start listening, or return the port it is already on.
    ///
    /// Binds the configured port when it is free and any free port when it is not: a
    /// second copy of the application, or an unrelated program on 47123, should not
    /// leave the user with a server that silently never started.
    pub async fn start(
        &self,
        config: &McpConfig,
        history: History,
        tokens: Arc<Mutex<TokenStore>>,
    ) -> Result<u16> {
        if let Some(port) = self.port() {
            return Ok(port);
        }

        let listener = bind(config.port).await?;
        let port = listener.local_addr()?.port();

        let state = ServerState {
            history,
            tokens,
            allow_history: config.allow_history,
        };

        let (shutdown, wait) = oneshot::channel();
        let task = tokio::spawn(async move {
            let served = axum::serve(listener, router(state))
                .with_graceful_shutdown(async {
                    let _ = wait.await;
                })
                .await;
            if let Err(error) = served {
                tracing::error!(%error, "the MCP server stopped unexpectedly");
            }
        });

        *self.running.lock() = Some(Running {
            port,
            shutdown,
            task,
        });
        tracing::info!(port, "MCP server listening on loopback");
        Ok(port)
    }

    /// Stop listening. Waits for the listener to close before returning.
    pub async fn stop(&self) {
        let Some(running) = self.running.lock().take() else {
            return;
        };
        let _ = running.shutdown.send(());
        let _ = running.task.await;
        tracing::info!(port = running.port, "MCP server stopped");
    }
}

/// Bind the preferred port, falling back to any free one.
async fn bind(preferred: u16) -> Result<TcpListener> {
    if let Ok(listener) = TcpListener::bind(SocketAddr::from((ADDRESS, preferred))).await {
        return Ok(listener);
    }
    tracing::warn!(
        preferred,
        "the preferred MCP port is taken; using a free one"
    );
    TcpListener::bind(SocketAddr::from((ADDRESS, 0)))
        .await
        .context("could not bind a port on the loopback interface")
}

/// Every route the server answers.
pub fn router(state: ServerState) -> Router {
    let protected = Router::new()
        .route("/mcp/v1/today", get(today))
        .route("/mcp/v1/summary/{date}", get(summary))
        .route("/mcp/v1/day/{date}", get(day))
        .route("/mcp/v1/search", get(search))
        .route("/mcp/v1/recent", get(recent))
        .route("/mcp", post(rpc_endpoint))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate));

    Router::new()
        .route("/mcp/v1/health", get(health))
        .merge(protected)
        .with_state(state)
}

/// A refusal, in the shape every route uses.
struct Refusal(StatusCode, String);

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

type Answer<T> = std::result::Result<Json<T>, Refusal>;

/// Reject anything without a token this server issued.
async fn authenticate(
    State(state): State<ServerState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let presented = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(tokens::from_header);

    let accepted = presented.is_some_and(|token| state.tokens.lock().accepts(token));
    if !accepted {
        return (
            StatusCode::UNAUTHORIZED,
            [("www-authenticate", "Bearer")],
            Json(json!({
                "error": "a bearer token issued by OpenHistory is required",
            })),
        )
            .into_response();
    }
    next.run(request).await
}

/// Enough to tell that this port belongs to OpenHistory, and nothing more.
async fn health() -> Json<Value> {
    Json(json!({
        "ok": true,
        "name": "openhistory",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn today(State(state): State<ServerState>) -> Answer<crate::DayView> {
    let date = oh_core::today();
    state
        .history
        .day(date, true)
        .map(Json)
        .map_err(internal_error)
}

async fn day(State(state): State<ServerState>, Path(date): Path<String>) -> Answer<crate::DayView> {
    let date = state.allowed_date(&date)?;
    state
        .history
        .day(date, true)
        .map(Json)
        .map_err(internal_error)
}

/// A day without its episodes: the summaries and the measurements only.
async fn summary(
    State(state): State<ServerState>,
    Path(date): Path<String>,
) -> Answer<crate::DayView> {
    let date = state.allowed_date(&date)?;
    state
        .history
        .day(date, false)
        .map(Json)
        .map_err(internal_error)
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default, alias = "query")]
    q: String,
    limit: Option<usize>,
}

async fn search(
    State(state): State<ServerState>,
    Query(query): Query<SearchQuery>,
) -> Answer<Value> {
    if query.q.trim().is_empty() {
        return Err(Refusal(
            StatusCode::BAD_REQUEST,
            "a search needs a query: ?q=something".into(),
        ));
    }
    let results = state
        .history
        .search(&query.q, query.limit.unwrap_or(50).clamp(1, MAX_LIMIT));

    Ok(Json(json!({ "query": query.q, "results": results })))
}

#[derive(Deserialize)]
struct RecentQuery {
    #[serde(default, alias = "count")]
    n: Option<usize>,
}

async fn recent(
    State(state): State<ServerState>,
    Query(query): Query<RecentQuery>,
) -> Answer<Value> {
    let episodes = state
        .history
        .recent(query.n.unwrap_or(10).clamp(1, MAX_LIMIT), oh_core::today())
        .map_err(internal_error)?;

    Ok(Json(json!({ "episodes": episodes })))
}

async fn rpc_endpoint(State(state): State<ServerState>, body: String) -> Response {
    rpc::handle(&state, &body).await
}

impl ServerState {
    /// Parse a date, and refuse days the settings put out of reach.
    fn allowed_date(&self, date: &str) -> std::result::Result<NaiveDate, Refusal> {
        let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| {
            Refusal(
                StatusCode::BAD_REQUEST,
                format!("{date} is not a date in YYYY-MM-DD form"),
            )
        })?;

        if !self.allow_history && parsed != oh_core::today() {
            return Err(Refusal(
                StatusCode::FORBIDDEN,
                "this server is limited to today. Turn on earlier days in Settings.".into(),
            ));
        }
        Ok(parsed)
    }
}

fn internal_error(error: anyhow::Error) -> Refusal {
    tracing::warn!(%error, "an MCP request could not be answered");
    Refusal(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Fixture, get, get_with_token, post_rpc};

    #[tokio::test]
    async fn health_answers_without_a_token_and_says_nothing_about_the_history() {
        let fixture = Fixture::start().await;
        let (status, body) = get(fixture.port(), "/mcp/v1/health").await;

        assert_eq!(status, 200);
        assert!(body.contains("openhistory"));
        assert!(!body.contains("Slack"), "{body}");
    }

    #[tokio::test]
    async fn every_other_route_refuses_a_request_with_no_token() {
        let fixture = Fixture::start().await;
        for path in [
            "/mcp/v1/today",
            "/mcp/v1/recent",
            "/mcp/v1/search?q=code",
            "/mcp/v1/summary/2026-08-22",
        ] {
            let (status, body) = get(fixture.port(), path).await;
            assert_eq!(status, 401, "{path}");
            assert!(!body.contains("Slack"), "{path}: {body}");
        }

        let (status, _) = post_rpc(fixture.port(), None, r#"{"method":"tools/list"}"#).await;
        assert_eq!(status, 401);
    }

    #[tokio::test]
    async fn a_wrong_token_is_refused() {
        let fixture = Fixture::start().await;
        let (status, _) =
            get_with_token(fixture.port(), "/mcp/v1/today", "oh_not-the-right-one").await;
        assert_eq!(status, 401);
    }

    #[tokio::test]
    async fn the_right_token_gets_the_day() {
        let fixture = Fixture::start().await;
        let (status, body) =
            get_with_token(fixture.port(), &fixture.today_path(), fixture.token()).await;

        assert_eq!(status, 200);
        assert!(body.contains("Visual Studio Code"), "{body}");
        assert!(body.contains("episodes"), "{body}");
    }

    #[tokio::test]
    async fn nothing_private_leaves_over_the_wire() {
        let fixture = Fixture::start().await;
        let (_, body) =
            get_with_token(fixture.port(), &fixture.today_path(), fixture.token()).await;

        assert!(!body.contains("nobody should see"), "{body}");
        assert!(!body.contains("token=abc"), "{body}");
        assert!(!body.contains(".exe"), "{body}");
    }

    #[tokio::test]
    async fn a_summary_route_carries_no_episodes() {
        let fixture = Fixture::start().await;
        let path = format!("/mcp/v1/summary/{}", fixture.date());
        let (status, body) = get_with_token(fixture.port(), &path, fixture.token()).await;

        assert_eq!(status, 200);
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert!(parsed.get("episodes").is_none(), "{body}");
        assert!(parsed["activeMs"].as_i64().unwrap() > 0, "{body}");
        assert!(
            parsed["screenMs"].as_i64().unwrap() >= parsed["activeMs"].as_i64().unwrap(),
            "{body}"
        );
        assert!(parsed["episodeCount"].as_u64().unwrap() > 0, "{body}");
    }

    #[tokio::test]
    async fn search_needs_a_query_and_answers_one() {
        let fixture = Fixture::start().await;
        let (status, _) = get_with_token(fixture.port(), "/mcp/v1/search", fixture.token()).await;
        assert_eq!(status, 400);

        let (status, body) =
            get_with_token(fixture.port(), "/mcp/v1/search?q=slack", fixture.token()).await;
        assert_eq!(status, 200);
        assert!(body.contains("Slack"), "{body}");
    }

    #[tokio::test]
    async fn recent_honours_the_count_it_is_given() {
        let fixture = Fixture::start().await;
        let (status, body) =
            get_with_token(fixture.port(), "/mcp/v1/recent?n=1", fixture.token()).await;

        assert_eq!(status, 200);
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["episodes"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_malformed_date_is_refused_before_anything_is_read() {
        let fixture = Fixture::start().await;
        let (status, body) =
            get_with_token(fixture.port(), "/mcp/v1/day/yesterday", fixture.token()).await;

        assert_eq!(status, 400);
        assert!(body.contains("YYYY-MM-DD"), "{body}");
    }

    #[tokio::test]
    async fn a_server_limited_to_today_refuses_earlier_days() {
        let fixture = Fixture::start_with(false).await;
        let (status, body) =
            get_with_token(fixture.port(), "/mcp/v1/day/2020-01-01", fixture.token()).await;

        assert_eq!(status, 403);
        assert!(body.contains("Settings"), "{body}");

        // Today still answers.
        let (status, _) =
            get_with_token(fixture.port(), &fixture.today_path(), fixture.token()).await;
        assert_eq!(status, 200);
    }

    #[tokio::test]
    async fn a_second_server_lands_on_a_different_port_rather_than_failing() {
        let first = Fixture::start().await;
        let second = Fixture::start_on(first.port()).await;

        assert_ne!(second.port(), first.port());
        let (status, _) = get(second.port(), "/mcp/v1/health").await;
        assert_eq!(status, 200);
    }

    #[tokio::test]
    async fn a_stopped_server_stops_answering() {
        let fixture = Fixture::start().await;
        let port = fixture.port();
        fixture.stop().await;

        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err(),
            "the port is still open after stop()"
        );
    }
}
