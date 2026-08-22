//! The local MCP server.
//!
//! An authenticated HTTP server on the loopback interface that lets another program —
//! Claude Code, an editor extension, any MCP client — ask what this machine has been
//! used for. It is off until the user turns it on, it binds `127.0.0.1` only, and
//! every route but the health check needs a bearer token this application issued.
//!
//! Two shapes over one set of answers: REST routes under `/mcp/v1/` for a shell
//! script, and JSON-RPC at `/mcp` for an MCP client. Both read through [`History`],
//! which reduces every episode the same way the inference layer does, so neither can
//! say more than the other.

pub mod history;
pub mod rpc;
pub mod server;
pub mod tokens;

#[cfg(test)]
mod testing;

pub use history::{DayView, History, SearchResult};
pub use server::{McpServer, McpStatus, ServerState};
pub use tokens::{StoredToken, TokenStore};
