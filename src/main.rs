//! redmine-mcp-rs: a Model Context Protocol server for Redmine.
//!
//! Speaks MCP over stdio (newline-delimited JSON-RPC 2.0) and translates tool
//! calls into Redmine REST API requests. Behaviour mirrors the Node reference
//! implementation, including admin-key impersonation, fail-closed safety, the
//! numeric-zero filter fix, and display-name resolution.

mod app;
mod tools;

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use app::{AppState, Config};

const PROTOCOL_VERSION: &str = "2024-11-05";

#[tokio::main]
async fn main() {
    let cfg = Config::from_env();

    if cfg.base_url.is_empty() {
        eprintln!("[redmine-mcp-rs] REDMINE_URL is not set");
    }
    if cfg.api_key.is_empty() {
        eprintln!("[redmine-mcp-rs] REDMINE_API_KEY is not set");
    }
    if cfg.lock_on_behalf_of {
        if !cfg.on_behalf_of.is_empty() {
            eprintln!(
                "[redmine-mcp-rs] Identity locked to '{}'; per-call on_behalf_of is disabled.",
                cfg.on_behalf_of
            );
        } else {
            eprintln!(
                "[redmine-mcp-rs] REDMINE_LOCK_ON_BEHALF_OF is set but REDMINE_ON_BEHALF_OF is empty; \
                 requests act as the API key owner and impersonation is disabled."
            );
        }
    }
    if cfg.allow_admin {
        eprintln!(
            "[redmine-mcp-rs] REDMINE_ALLOW_ADMIN is set; tool calls may run with full admin \
             privileges when no impersonation identity is in effect."
        );
    }

    let lock = cfg.lock_on_behalf_of;
    let state = Arc::new(AppState::new(cfg));

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    while let Ok(Some(line)) = reader.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Only requests (with a method) get a response; ignore responses/acks.
        let method = match msg.get("method").and_then(|m| m.as_str()) {
            Some(m) => m.to_string(),
            None => continue,
        };
        let id = msg.get("id").cloned();

        if let Some(response) = handle_message(&state, lock, &method, id, &msg).await {
            let mut buf = serde_json::to_vec(&response).unwrap();
            buf.push(b'\n');
            if stdout.write_all(&buf).await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    }
}

async fn handle_message(
    state: &Arc<AppState>,
    lock: bool,
    method: &str,
    id: Option<Value>,
    msg: &Value,
) -> Option<Value> {
    // Notifications (no id) never get a reply.
    match method {
        "notifications/initialized" | "notifications/cancelled" => return None,
        _ => {}
    }
    let id = id?; // requests without an id are notifications -> no reply

    match method {
        "initialize" => Some(result(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "redmine-mcp-rs", "version": env!("CARGO_PKG_VERSION") }
            }),
        )),
        "ping" => Some(result(id, json!({}))),
        "tools/list" => Some(result(id, json!({ "tools": tools::tools(lock) }))),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let name = params
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string();
            let args = params
                .get("arguments")
                .and_then(|a| a.as_object())
                .cloned()
                .unwrap_or_default();
            let tool_result = state.call_tool(&name, args).await;
            Some(result(id, tool_result))
        }
        _ => Some(error(id, -32601, &format!("Method not found: {method}"))),
    }
}

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
