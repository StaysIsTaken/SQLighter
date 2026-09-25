//! Local MCP server (Model Context Protocol, Streamable HTTP transport, JSON responses).
//!
//! Lets Claude Code, Claude Desktop / Cowork (through the stdio bridge) and other MCP clients use
//! SQLighter's connections with the same guard rails as the built-in assistant.
//! Security:
//!  * binds to 127.0.0.1 only,
//!  * every request needs a bearer token (256 bit, compared in constant time),
//!  * Host and Origin headers are checked (protection against DNS rebinding / browser access),
//!  * data access follows the AI access level; data-modifying SQL is off by default and every
//!    statement needs explicit approval in the SQLighter window.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State as AxState};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use tauri::{Manager, State};
use tokio::sync::oneshot;

use crate::ai::tools::{self, Scope};
use crate::error::AppResult;
use crate::model::{AiAccessLevel, McpInfo};
use crate::state::AppState;

const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const ENDPOINT_FILE: &str = "mcp-endpoint.json";
const TOKEN_KEY: &str = "mcp:token";

#[derive(Clone)]
enum Grant {
    /// External clients (configured by the user). Scope follows the current settings.
    External,
    /// Short-lived token for one Claude Code chat.
    Scoped(Scope),
}

struct Running {
    port: u16,
    external: bool,
    shutdown: Option<oneshot::Sender<()>>,
}

#[derive(Default)]
pub struct McpState {
    running: tokio::sync::Mutex<Option<Running>>,
    tokens: Mutex<HashMap<String, Grant>>,
}

fn new_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::rng().fill_bytes(&mut b);
    hex::encode(b)
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn external_token(state: &AppState) -> String {
    match state.store.secret(TOKEN_KEY) {
        Some(t) if t.len() == 64 => t,
        _ => {
            let t = new_token();
            state.store.set_secret(TOKEN_KEY, &t);
            t
        }
    }
}

/// Starts/stops the externally reachable server according to the settings.
pub async fn apply_settings(state: Arc<AppState>) -> Result<()> {
    let s = state.store.settings();
    let endpoint_path = state.store.dir().join(ENDPOINT_FILE);
    if s.mcp_enabled {
        let token = external_token(&state);
        {
            let mut t = state.mcp.tokens.lock().unwrap();
            t.retain(|_, g| !matches!(g, Grant::External));
            t.insert(token.clone(), Grant::External);
        }
        let mut r = state.mcp.running.lock().await;
        let restart = r.as_ref().is_none_or(|x| x.port != s.mcp_port);
        if restart {
            if let Some(mut old) = r.take() {
                if let Some(tx) = old.shutdown.take() {
                    let _ = tx.send(());
                }
            }
            *r = Some(start(state.clone(), s.mcp_port, true).await?);
        } else if let Some(x) = r.as_mut() {
            x.external = true;
        }
        let url = format!("http://127.0.0.1:{}/mcp", s.mcp_port);
        crate::store::write_json(&endpoint_path, &json!({"url": url, "token": token}))?;
    } else {
        state.mcp.tokens.lock().unwrap().retain(|_, g| !matches!(g, Grant::External));
        let _ = std::fs::remove_file(&endpoint_path);
        let mut r = state.mcp.running.lock().await;
        if r.as_ref().is_some_and(|x| x.external) {
            if let Some(mut old) = r.take() {
                if let Some(tx) = old.shutdown.take() {
                    let _ = tx.send(());
                }
            }
        }
    }
    Ok(())
}

async fn start(state: Arc<AppState>, port: u16, external: bool) -> Result<Running> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.with_context(|| format!("MCP server: port {port} is not available"))?;
    let port = listener.local_addr()?.port();
    let (tx, rx) = oneshot::channel::<()>();
    let app = Router::new()
        .route("/mcp", post(handle).get(|| async { StatusCode::METHOD_NOT_ALLOWED }).delete(|| async { StatusCode::METHOD_NOT_ALLOWED }))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state((state, port));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = rx.await;
        })
        .await;
    });
    Ok(Running { port, external, shutdown: Some(tx) })
}

/// Creates a token restricted to `scope` (used for Claude Code chats). Starts an internal
/// server on a random loopback port if the external one is disabled.
pub async fn ephemeral_token(state: Arc<AppState>, scope: Scope) -> Result<(String, String)> {
    let port = {
        let mut r = state.mcp.running.lock().await;
        if r.is_none() {
            *r = Some(start(state.clone(), 0, false).await?);
        }
        r.as_ref().unwrap().port
    };
    let token = new_token();
    state.mcp.tokens.lock().unwrap().insert(token.clone(), Grant::Scoped(scope));
    Ok((format!("http://127.0.0.1:{port}/mcp"), token))
}

pub fn revoke_token(state: &AppState, token: &str) {
    state.mcp.tokens.lock().unwrap().remove(token);
}

fn rpc_error(id: Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

async fn handle(AxState((state, port)): AxState<(Arc<AppState>, u16)>, headers: HeaderMap, body: Bytes) -> Response {
    // DNS rebinding protection: only loopback host names with our port.
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return (StatusCode::FORBIDDEN, "invalid Host header").into_response();
    }
    // Browsers always send Origin for cross-site requests; MCP clients don't.
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) {
        if !(origin.starts_with("http://127.0.0.1") || origin.starts_with("http://localhost")) {
            return (StatusCode::FORBIDDEN, "cross-origin requests are not allowed").into_response();
        }
    }
    let token = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok()).and_then(|h| h.strip_prefix("Bearer ")).unwrap_or("").trim().to_string();
    let grant = {
        let tokens = state.mcp.tokens.lock().unwrap();
        let mut found = None;
        for (k, g) in tokens.iter() {
            if ct_eq(k, &token) {
                found = Some(g.clone());
            }
        }
        found
    };
    let Some(grant) = grant else {
        return (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")], "invalid or missing token").into_response();
    };
    let scope = match grant {
        Grant::Scoped(s) => s,
        Grant::External => {
            let st = state.store.settings();
            Scope {
                connection_id: None,
                access: if st.ai_access == AiAccessLevel::None { AiAccessLevel::Schema } else { st.ai_access },
                allow_write: st.mcp_allow_write,
                allow_open_editor: true,
                label: "MCP client".into(),
            }
        }
    };
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return json_response(rpc_error(Value::Null, -32700, "parse error"));
    };
    if req.is_array() {
        return json_response(rpc_error(Value::Null, -32600, "batch requests are not supported"));
    }
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let Some(id) = id else {
        // Notification (e.g. notifications/initialized)
        return StatusCode::ACCEPTED.into_response();
    };
    let params = req.get("params").cloned().unwrap_or(json!({}));
    let result = match method {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOLS[0]);
            let version = if PROTOCOLS.contains(&requested) { requested } else { PROTOCOLS[0] };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "sqlighter", "title": "SQLighter", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "SQLighter gives access to the user's database connections. Inspect schemas with list_tables / describe_table before writing SQL. \
                                 Propose SQL to the user; use open_in_editor to hand longer scripts over for review."
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => {
            let list: Vec<Value> = tools::definitions(&scope).into_iter().map(|t| json!({"name": t.name, "description": t.description, "inputSchema": t.schema})).collect();
            Ok(json!({"tools": list}))
        }
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match tools::call(&state, &scope, name, &args).await {
                Ok(text) => Ok(json!({"content": [{"type": "text", "text": text}], "isError": false})),
                Err(e) => Ok(json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true})),
            }
        }
        "resources/list" => Ok(json!({"resources": []})),
        "prompts/list" => Ok(json!({"prompts": []})),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    match result {
        Ok(r) => json_response(json!({"jsonrpc": "2.0", "id": id, "result": r})),
        Err((code, msg)) => json_response(rpc_error(id, code, &msg)),
    }
}

fn json_response(v: Value) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response()
}

fn bridge_path(state: &AppState) -> String {
    let res = state.app.path().resource_dir().map(|d| d.join("sqlighter-mcp-bridge.mjs")).ok();
    match res {
        Some(p) if p.exists() => p.to_string_lossy().into_owned(),
        _ => state.store.dir().join("sqlighter-mcp-bridge.mjs").to_string_lossy().into_owned(),
    }
}

#[tauri::command]
pub async fn mcp_info(state: State<'_, Arc<AppState>>) -> AppResult<McpInfo> {
    let s = state.store.settings();
    let running = state.mcp.running.lock().await.as_ref().is_some_and(|r| r.external);
    let token = if s.mcp_enabled { external_token(&state) } else { String::new() };
    let url = format!("http://127.0.0.1:{}/mcp", s.mcp_port);
    // Make sure the bridge script exists at a stable location.
    let bridge = bridge_path(&state);
    if !std::path::Path::new(&bridge).exists() {
        let _ = std::fs::write(&bridge, include_str!("../resources/sqlighter-mcp-bridge.mjs"));
    }
    let endpoint = state.store.dir().join(ENDPOINT_FILE).to_string_lossy().into_owned();
    let desktop = json!({"mcpServers": {"sqlighter": {"command": "node", "args": [bridge], "env": {"SQLIGHTER_MCP_ENDPOINT": endpoint}}}});
    Ok(McpInfo {
        enabled: s.mcp_enabled,
        running,
        claude_code_command: format!("claude mcp add --transport http sqlighter {url} --header \"Authorization: Bearer {token}\""),
        claude_desktop_config: serde_json::to_string_pretty(&desktop).unwrap_or_default(),
        url,
        token,
        bridge_path: bridge,
    })
}

#[tauri::command]
pub async fn mcp_regenerate_token(state: State<'_, Arc<AppState>>) -> AppResult<McpInfo> {
    state.store.set_secret(TOKEN_KEY, &new_token());
    apply_settings(state.inner().clone()).await?;
    mcp_info(state).await
}
