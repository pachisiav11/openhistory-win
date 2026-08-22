//! The MCP endpoint, spoken as JSON-RPC 2.0 over one POST.
//!
//! Four tools, each a thin wrapper over [`crate::History`]: today, a named day, a
//! search, and the most recent sessions. A tool result is the same JSON the REST route
//! returns, carried in a text block, because that is what MCP clients read.
//!
//! The protocol version is echoed back rather than asserted. A client that opens with
//! a version this server has never heard of gets that version agreed to, and every
//! method below behaves the same under all of them; refusing the handshake over a date
//! string would break clients for no gain.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::server::ServerState;

/// Sent when the client does not name a version.
const DEFAULT_PROTOCOL: &str = "2025-06-18";

const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

#[derive(Debug, Deserialize)]
struct RpcRequest {
    /// Absent on a notification, which takes no reply.
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

/// Answer one JSON-RPC message.
pub async fn handle(state: &ServerState, body: &str) -> Response {
    let request: RpcRequest = match serde_json::from_str(body) {
        Ok(request) => request,
        Err(error) => {
            return reply(error_object(
                Value::Null,
                PARSE_ERROR,
                &format!("could not read the request: {error}"),
            ));
        }
    };

    let Some(id) = request.id.clone() else {
        // A notification. `notifications/initialized` is the one that matters, and the
        // protocol says a notification gets no response body at all.
        return StatusCode::ACCEPTED.into_response();
    };

    match dispatch(state, &request).await {
        Ok(result) => reply(json!({ "jsonrpc": "2.0", "id": id, "result": result })),
        Err(RpcError { code, message }) => reply(error_object(id, code, &message)),
    }
}

struct RpcError {
    code: i32,
    message: String,
}

impl RpcError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
        }
    }
}

type RpcResult = std::result::Result<Value, RpcError>;

async fn dispatch(state: &ServerState, request: &RpcRequest) -> RpcResult {
    match request.method.as_str() {
        "initialize" => Ok(initialize(&request.params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools(state) })),
        "tools/call" => call_tool(state, &request.params),
        // Declared unsupported rather than answered with an empty list, so a client
        // does not show this server as offering resources or prompts it has none of.
        other => Err(RpcError::new(
            METHOD_NOT_FOUND,
            format!("this server does not implement {other}"),
        )),
    }
}

fn initialize(params: &Value) -> Value {
    let protocol = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL);

    json!({
        "protocolVersion": protocol,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "openhistory",
            "title": "OpenHistory",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": "Ask what this computer has been used for. Times are local. \
    Sessions marked private carry an application name and a span of time only, and nothing \
    about them may be guessed at.",
    })
}

fn tools(state: &ServerState) -> Vec<Value> {
    let day_range = if state.allow_history {
        "A date in YYYY-MM-DD form."
    } else {
        "A date in YYYY-MM-DD form. This server is limited to today."
    };

    vec![
        json!({
            "name": "get_today",
            "title": "Today's activity",
            "description": "What this computer has been used for today: the sessions, \
        the time spent in each application, and any summaries that have been written.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        }),
        json!({
            "name": "get_day",
            "title": "A day's activity",
            "description": "The same for a named day.",
            "inputSchema": {
                "type": "object",
                "properties": { "date": { "type": "string", "description": day_range } },
                "required": ["date"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "search_history",
            "title": "Search the history",
            "description": "Find sessions whose application or window title matches \
        every word of a query. Private sessions can be found by application, and come back \
        without their titles.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200 },
                },
                "required": ["query"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "get_recent",
            "title": "Recent sessions",
            "description": "The most recent sessions, newest first, looking back over \
        the last week.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "count": { "type": "integer", "minimum": 1, "maximum": 200 },
                },
                "additionalProperties": false,
            },
        }),
    ]
}

fn call_tool(state: &ServerState, params: &Value) -> RpcResult {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "a tool call needs a name"))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    // A tool that fails reports it inside the result rather than as a protocol error:
    // the client is meant to show the model what went wrong, not drop the turn.
    match run_tool(state, name, &arguments) {
        Ok(value) => Ok(content(&value, false)),
        Err(RpcError { code, message }) if code == METHOD_NOT_FOUND => {
            Err(RpcError::new(METHOD_NOT_FOUND, message))
        }
        Err(RpcError { message, .. }) => Ok(content(&json!({ "error": message }), true)),
    }
}

fn run_tool(state: &ServerState, name: &str, arguments: &Value) -> RpcResult {
    match name {
        "get_today" => {
            let view = state
                .history
                .day(oh_core::today(), true)
                .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string()))?;
            to_value(&view)
        }
        "get_day" => {
            let date = arguments
                .get("date")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::new(INVALID_PARAMS, "get_day needs a date"))?;
            let date = parse_allowed(state, date)?;

            let view = state
                .history
                .day(date, true)
                .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string()))?;
            to_value(&view)
        }
        "search_history" => {
            let query = arguments
                .get("query")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .ok_or_else(|| RpcError::new(INVALID_PARAMS, "search_history needs a query"))?;
            let limit = count(arguments, "limit", 50);

            to_value(&json!({
                "query": query,
                "results": state.history.search(query, limit),
            }))
        }
        "get_recent" => {
            let episodes = state
                .history
                .recent(count(arguments, "count", 10), oh_core::today())
                .map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string()))?;
            to_value(&json!({ "episodes": episodes }))
        }
        other => Err(RpcError::new(
            METHOD_NOT_FOUND,
            format!("this server has no tool called {other}"),
        )),
    }
}

fn parse_allowed(
    state: &ServerState,
    date: &str,
) -> std::result::Result<chrono::NaiveDate, RpcError> {
    let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| {
        RpcError::new(
            INVALID_PARAMS,
            format!("{date} is not a date in YYYY-MM-DD form"),
        )
    })?;

    if !state.allow_history && parsed != oh_core::today() {
        return Err(RpcError::new(
            INVALID_REQUEST,
            "this server is limited to today",
        ));
    }
    Ok(parsed)
}

fn count(arguments: &Value, key: &str, fallback: usize) -> usize {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(fallback)
        .clamp(1, 200)
}

fn to_value<T: serde::Serialize>(value: &T) -> RpcResult {
    serde_json::to_value(value).map_err(|error| RpcError::new(INTERNAL_ERROR, error.to_string()))
}

/// A tool result, as MCP expects it: text blocks the client hands to the model.
fn content(value: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

fn error_object(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

fn reply(body: Value) -> Response {
    axum::Json(body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Fixture, post_rpc};

    async fn rpc(fixture: &Fixture, body: &str) -> Value {
        let (status, text) = post_rpc(fixture.port(), Some(fixture.token()), body).await;
        assert_eq!(status, 200, "{text}");
        serde_json::from_str(&text).unwrap()
    }

    /// The text a tool call carries, parsed back into JSON.
    fn tool_payload(response: &Value) -> Value {
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    #[tokio::test]
    async fn the_handshake_agrees_to_the_version_the_client_asked_for() {
        let fixture = Fixture::start().await;
        let response = rpc(
            &fixture,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
        )
        .await;

        assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(response["result"]["serverInfo"]["name"], "openhistory");
        assert!(response["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn a_client_that_names_no_version_gets_the_default() {
        let fixture = Fixture::start().await;
        let response = rpc(
            &fixture,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        )
        .await;
        assert_eq!(response["result"]["protocolVersion"], DEFAULT_PROTOCOL);
    }

    #[tokio::test]
    async fn a_notification_is_accepted_with_no_reply() {
        let fixture = Fixture::start().await;
        let (status, body) = post_rpc(
            fixture.port(),
            Some(fixture.token()),
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )
        .await;

        assert_eq!(status, 202);
        assert!(body.trim().is_empty(), "{body}");
    }

    #[tokio::test]
    async fn the_tool_list_names_four_tools_with_schemas() {
        let fixture = Fixture::start().await;
        let response = rpc(
            &fixture,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        )
        .await;

        let tools = response["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["get_today", "get_day", "search_history", "get_recent"]
        );
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(!tool["description"].as_str().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn calling_get_today_returns_the_day_as_json_text() {
        let fixture = Fixture::start().await;
        let response = rpc(
            &fixture,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_today","arguments":{}}}"#,
        )
        .await;

        assert_eq!(response["result"]["isError"], false);
        let payload = tool_payload(&response);
        assert_eq!(payload["date"], fixture.date());
        assert!(payload["episodes"].is_array());
    }

    #[tokio::test]
    async fn nothing_private_reaches_a_tool_result() {
        let fixture = Fixture::start().await;
        let response = rpc(
            &fixture,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_today"}}"#,
        )
        .await;

        let text = response.to_string();
        assert!(!text.contains("nobody should see"), "{text}");
        assert!(!text.contains("token=abc"), "{text}");
        assert!(!text.contains(".exe"), "{text}");
    }

    #[tokio::test]
    async fn a_search_call_finds_what_was_recorded() {
        let fixture = Fixture::start().await;
        let response = rpc(
            &fixture,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"search_history","arguments":{"query":"slack"}}}"#,
        )
        .await;

        let payload = tool_payload(&response);
        assert!(!payload["results"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_tool_called_with_bad_arguments_reports_it_in_the_result() {
        let fixture = Fixture::start().await;
        let response = rpc(
            &fixture,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"get_day","arguments":{"date":"yesterday"}}}"#,
        )
        .await;

        assert_eq!(response["result"]["isError"], true);
        assert!(
            tool_payload(&response)["error"]
                .as_str()
                .unwrap()
                .contains("YYYY-MM-DD")
        );
    }

    #[tokio::test]
    async fn a_server_limited_to_today_says_so_when_asked_for_another_day() {
        let fixture = Fixture::start_with(false).await;
        let response = rpc(
            &fixture,
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"get_day","arguments":{"date":"2020-01-01"}}}"#,
        )
        .await;

        assert_eq!(response["result"]["isError"], true);
        assert!(
            tool_payload(&response)["error"]
                .as_str()
                .unwrap()
                .contains("limited to today")
        );
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("limited to today")
        );
    }

    #[tokio::test]
    async fn an_unknown_tool_is_a_protocol_error() {
        let fixture = Fixture::start().await;
        let response = rpc(
            &fixture,
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"delete_everything"}}"#,
        )
        .await;

        assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn an_unknown_method_is_refused_rather_than_ignored() {
        let fixture = Fixture::start().await;
        let response = rpc(
            &fixture,
            r#"{"jsonrpc":"2.0","id":9,"method":"resources/list"}"#,
        )
        .await;

        assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_reported_as_a_parse_error() {
        let fixture = Fixture::start().await;
        let (status, body) = post_rpc(fixture.port(), Some(fixture.token()), "{ not json").await;

        assert_eq!(status, 200);
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["error"]["code"], PARSE_ERROR);
    }
}
