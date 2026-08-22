//! The IPC surface for the local MCP server.
//!
//! The server itself lives in `oh-mcp`. This module owns the one instance the
//! application runs, keeps it in step with the settings, and gives the settings page
//! the three things it needs: whether the server is up, where it is, and a token.
//!
//! A token is shown once. Only its hash is stored, so the settings page can display a
//! token it has just minted and never one from before a restart; the button offered
//! after that is Regenerate, not Reveal.

use std::sync::Arc;

use oh_core::{Config, McpConfig};
use oh_mcp::{History, McpServer, McpStatus, TokenStore};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::State;

use crate::{AppState, to_message};

/// The server, its tokens, and the history it reads.
pub struct McpState {
    server: Arc<McpServer>,
    tokens: Arc<Mutex<TokenStore>>,
    history: History,
    /// The settings the running server was started with, so a change to the port or to
    /// the history setting can restart it and an unrelated change cannot.
    started_with: Mutex<Option<McpConfig>>,
}

/// What a start or a status check tells the settings page.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpHandle {
    #[serde(flatten)]
    pub status: McpStatus,
    /// Present only when a token was minted just now. There is no way to show an
    /// existing one: the store keeps hashes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl McpState {
    pub fn new(history: History, tokens: TokenStore) -> Self {
        McpState {
            server: Arc::new(McpServer::new()),
            tokens: Arc::new(Mutex::new(tokens)),
            history,
            started_with: Mutex::new(None),
        }
    }

    pub fn server(&self) -> Arc<McpServer> {
        Arc::clone(&self.server)
    }

    fn has_token(&self) -> bool {
        !self.tokens.lock().is_empty()
    }

    pub fn status(&self) -> McpStatus {
        self.server.status(self.has_token())
    }

    /// Start the server, minting a first token if there is none.
    async fn start(&self, config: &McpConfig) -> Result<McpHandle, String> {
        let token = self.tokens.lock().ensure_one().map_err(to_message)?;
        self.server
            .start(config, self.history.clone(), Arc::clone(&self.tokens))
            .await
            .map_err(to_message)?;
        *self.started_with.lock() = Some(config.clone());
        Ok(McpHandle {
            status: self.status(),
            token,
        })
    }

    async fn stop(&self) {
        self.server.stop().await;
        *self.started_with.lock() = None;
    }

    /// Make the running server match the settings.
    ///
    /// A port or history change needs a restart, because both are read once when the
    /// server binds. Anything else leaves a running server alone.
    pub async fn reconcile(&self, config: &McpConfig) -> Result<McpStatus, String> {
        if !config.enabled {
            self.stop().await;
            return Ok(self.status());
        }

        let unchanged = self.started_with.lock().as_ref() == Some(config);
        if unchanged && self.server.is_running() {
            return Ok(self.status());
        }

        self.stop().await;
        self.start(config).await.map(|handle| handle.status)
    }
}

#[tauri::command]
pub fn mcp_status(state: State<'_, McpState>) -> McpStatus {
    state.status()
}

/// Turn the server on and remember that it should be on.
#[tauri::command]
pub async fn start_mcp(
    app: State<'_, AppState>,
    state: State<'_, McpState>,
) -> Result<McpHandle, String> {
    let mut config = app.config();
    if !config.mcp.enabled {
        config.mcp.enabled = true;
        app.apply(config.clone())?;
    }
    state.start(&config.mcp).await
}

#[tauri::command]
pub async fn stop_mcp(
    app: State<'_, AppState>,
    state: State<'_, McpState>,
) -> Result<McpStatus, String> {
    let mut config = app.config();
    if config.mcp.enabled {
        config.mcp.enabled = false;
        app.apply(config)?;
    }
    state.stop().await;
    Ok(state.status())
}

/// Mint a new token and discard every earlier one. Shown once.
#[tauri::command]
pub fn regenerate_mcp_token(state: State<'_, McpState>) -> Result<String, String> {
    state.tokens.lock().regenerate(None).map_err(to_message)
}

/// Discard every token. The server keeps listening and accepts nothing.
#[tauri::command]
pub fn forget_mcp_tokens(state: State<'_, McpState>) -> Result<McpStatus, String> {
    state.tokens.lock().clear().map_err(to_message)?;
    Ok(state.status())
}

/// The block to paste into an MCP client's configuration.
///
/// Built here rather than in the window so the address and the header name are stated
/// once. The token is only known to the caller right after it was minted; without one
/// the snippet carries a placeholder.
#[tauri::command]
pub fn mcp_client_config(
    token: Option<String>,
    state: State<'_, McpState>,
) -> Result<String, String> {
    let status = state.status();
    let url = status
        .url
        .ok_or("the server is not running, so there is no address to give a client")?;
    let token = token.unwrap_or_else(|| "YOUR_TOKEN".to_owned());

    let snippet = serde_json::json!({
        "mcpServers": {
            "openhistory": {
                "type": "http",
                "url": format!("{url}/mcp"),
                "headers": { "Authorization": format!("Bearer {token}") },
            }
        }
    });
    serde_json::to_string_pretty(&snippet).map_err(to_message)
}

/// Start the server at launch if the settings say it should be running.
pub async fn start_if_enabled(state: &McpState, config: &Config) {
    if !config.mcp.enabled {
        return;
    }
    match state.start(&config.mcp).await {
        Ok(handle) => {
            tracing::info!(port = ?handle.status.port, "MCP server started at launch");
        }
        Err(error) => tracing::error!(%error, "could not start the MCP server at launch"),
    }
}
