//! A running server over a recorded day, for the tests in this crate.
//!
//! The fixture records today rather than a fixed date, because the routes under test
//! ask what today is. The events sit at midday local so the day is the same one in any
//! time zone.

use std::sync::Arc;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use oh_core::{
    ActivityEvent, ApplicationDescriptor, BrowserObservation, EventKind, EventStore, McpConfig,
};
use parking_lot::Mutex;

use crate::history::History;
use crate::server::McpServer;
use crate::tokens::TokenStore;

pub struct Fixture {
    _temp: tempfile::TempDir,
    server: Arc<McpServer>,
    port: u16,
    token: String,
    date: NaiveDate,
}

impl Fixture {
    pub async fn start() -> Self {
        Self::build(true, 0).await
    }

    pub async fn start_with(allow_history: bool) -> Self {
        Self::build(allow_history, 0).await
    }

    /// Start while `taken` is already bound, to exercise the port fallback.
    pub async fn start_on(taken: u16) -> Self {
        Self::build(true, taken).await
    }

    async fn build(allow_history: bool, preferred: u16) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let date = oh_core::today();

        let mut store = EventStore::in_dir(temp.path().join("events")).unwrap();
        for event in workday(date) {
            store.append(&event).unwrap();
        }

        let mut tokens = TokenStore::at(temp.path().join("tokens.json")).unwrap();
        let token = tokens.regenerate(None).unwrap();

        let server = Arc::new(McpServer::new());
        let config = McpConfig {
            enabled: true,
            port: preferred,
            allow_history,
        };
        let history = History::in_root(temp.path()).unwrap();
        // Process the day once, the way the window does when it is opened. The search
        // index is built as days are processed, so a server over an unprocessed
        // history would answer every search with nothing.
        history.day(date, true).unwrap();

        let port = server
            .start(&config, history, Arc::new(Mutex::new(tokens)))
            .await
            .unwrap();

        Fixture {
            _temp: temp,
            server,
            port,
            token,
            date,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn date(&self) -> String {
        self.date.format("%Y-%m-%d").to_string()
    }

    pub fn today_path(&self) -> String {
        "/mcp/v1/today".to_owned()
    }

    pub async fn stop(self) {
        self.server.stop().await;
    }
}

fn at(date: NaiveDate, minutes: i64) -> DateTime<Utc> {
    oh_processing::rollup::hour_start(date, 12).unwrap() + Duration::minutes(minutes)
}

fn app(name: &str) -> ApplicationDescriptor {
    ApplicationDescriptor {
        name: name.to_owned(),
        path: format!(r"C:\Programs\{name}.exe"),
        pid: 1,
        bundle_id: None,
    }
}

fn event(kind: EventKind, date: NaiveDate, minutes: i64, name: &str, title: &str) -> ActivityEvent {
    ActivityEvent::at(kind, at(date, minutes))
        .with_application(app(name))
        .with_window_title(title)
}

fn workday(date: NaiveDate) -> Vec<ActivityEvent> {
    vec![
        event(
            EventKind::ApplicationActivated,
            date,
            0,
            "Visual Studio Code",
            "history.rs - openhistory-win",
        ),
        event(
            EventKind::WindowChanged,
            date,
            10,
            "Visual Studio Code",
            "server.rs - openhistory-win",
        ),
        event(
            EventKind::ApplicationActivated,
            date,
            30,
            "Google Chrome",
            "A page nobody should see",
        )
        .with_browser(BrowserObservation {
            url: Some("https://example.com/secret?token=abc".into()),
            is_private: true,
        }),
        event(
            EventKind::WindowChanged,
            date,
            38,
            "Google Chrome",
            "Another page nobody should see",
        )
        .with_browser(BrowserObservation {
            url: Some("https://example.com/secret?token=abc".into()),
            is_private: true,
        }),
        event(
            EventKind::ApplicationActivated,
            date,
            60,
            "Slack",
            "#engineering - Slack",
        ),
        event(
            EventKind::WindowChanged,
            date,
            68,
            "Slack",
            "#design - Slack",
        ),
    ]
}

/// A GET with no credential.
pub async fn get(port: u16, path: &str) -> (u16, String) {
    request(port, path, None).await
}

pub async fn get_with_token(port: u16, path: &str, token: &str) -> (u16, String) {
    request(port, path, Some(token)).await
}

async fn request(port: u16, path: &str, token: Option<&str>) -> (u16, String) {
    let mut builder = reqwest::Client::new().get(format!("http://127.0.0.1:{port}{path}"));
    if let Some(token) = token {
        builder = builder.bearer_auth(token);
    }
    let response = builder.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.text().await.unwrap())
}

pub async fn post_rpc(port: u16, token: Option<&str>, body: &str) -> (u16, String) {
    let mut builder = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .header("content-type", "application/json")
        .body(body.to_owned());
    if let Some(token) = token {
        builder = builder.bearer_auth(token);
    }
    let response = builder.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.text().await.unwrap())
}
