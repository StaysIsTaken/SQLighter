//! AI provider protocol tests against local mock servers (no network, no API keys):
//! streaming text + one tool-call round trip for OpenAI-compatible, Ollama and Anthropic APIs.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use sqlighter_lib::ai::providers;
use sqlighter_lib::ai::tools::{definitions, Scope};
use sqlighter_lib::ai::{Emit, ToolRunner};
use sqlighter_lib::model::*;

type Log = Arc<Mutex<Vec<Value>>>;

async fn serve(router: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    format!("http://{addr}")
}

fn provider(kind: AiProviderKind, base: &str) -> AiProviderConfig {
    AiProviderConfig { id: "t".into(), name: "t".into(), kind, base_url: base.into(), model: "m".into(), allow_insecure_http: false, command: None, temperature: Some(0.0) }
}

fn scope() -> Scope {
    Scope { connection_id: Some("c".into()), access: AiAccessLevel::Schema, allow_write: false, allow_open_editor: false, label: "test".into() }
}

struct Capture {
    text: Arc<Mutex<String>>,
    tools: Arc<Mutex<Vec<String>>>,
}

fn capture() -> (Emit, Capture) {
    let text = Arc::new(Mutex::new(String::new()));
    let tools = Arc::new(Mutex::new(vec![]));
    let (t, u) = (text.clone(), tools.clone());
    let emit = Emit::new(Arc::new(move |k| match k {
        ChatEventKind::Text { text } => t.lock().unwrap().push_str(&text),
        ChatEventKind::Tool { name, .. } => u.lock().unwrap().push(name),
        _ => {}
    }));
    (emit, Capture { text, tools })
}

fn runner(calls: Arc<Mutex<Vec<(String, Value)>>>) -> Box<ToolRunner> {
    Box::new(move |name, args| {
        calls.lock().unwrap().push((name.clone(), args));
        Box::pin(async move { format!("result of {name}: users(id, name)") })
    })
}

fn history() -> Vec<ChatMessage> {
    vec![ChatMessage { role: "user".into(), content: "Show all users".into() }]
}

#[tokio::test]
async fn openai_streaming_with_tool_call() {
    let log: Log = Default::default();
    let l = log.clone();
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |body: Bytes| {
            let l = l.clone();
            async move {
                let v: Value = serde_json::from_slice(&body).unwrap();
                l.lock().unwrap().push(v.clone());
                let has_tool_result = v["messages"].as_array().unwrap().iter().any(|m| m["role"] == "tool");
                let chunks: Vec<Value> = if !has_tool_result {
                    vec![
                        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "list_tables", "arguments": ""}}]}}]}),
                        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"schema\":"}}]}}]}),
                        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "\"public\"}"}}]}, "finish_reason": "tool_calls"}]}),
                    ]
                } else {
                    vec![json!({"choices": [{"delta": {"content": "```sql\nSELECT * "}}]}), json!({"choices": [{"delta": {"content": "FROM users;\n```"}, "finish_reason": "stop"}]})]
                };
                let mut s = String::new();
                for c in chunks {
                    s.push_str(&format!("data: {c}\n\n"));
                }
                s.push_str("data: [DONE]\n\n");
                ([("content-type", "text/event-stream")], s)
            }
        }),
    );
    let base = serve(router).await;
    let (emit, cap) = capture();
    let calls: Arc<Mutex<Vec<(String, Value)>>> = Default::default();
    let r = runner(calls.clone());
    let defs = definitions(&scope());
    providers::openai(&provider(AiProviderKind::Openai, &format!("{base}/v1")), Some("sk-test".into()), "sys".into(), &history(), &defs, &r, &emit).await.unwrap();
    assert_eq!(calls.lock().unwrap()[0].0, "list_tables");
    assert_eq!(calls.lock().unwrap()[0].1, json!({"schema": "public"}));
    assert!(cap.text.lock().unwrap().contains("SELECT * FROM users;"));
    assert_eq!(cap.tools.lock().unwrap().as_slice(), ["list_tables"]);
    let reqs = log.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0]["messages"][0]["role"], "system");
    assert!(reqs[0]["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "describe_table"));
    // No write tool is ever offered to the built-in assistant
    assert!(!reqs[0]["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "execute_sql"));
    let msgs = reqs[1]["messages"].as_array().unwrap();
    assert_eq!(msgs[msgs.len() - 1]["tool_call_id"], "call_1");
}

#[tokio::test]
async fn ollama_ndjson_with_tool_call() {
    let router = Router::new().route(
        "/api/chat",
        post(|body: Bytes| async move {
            let v: Value = serde_json::from_slice(&body).unwrap();
            let has_tool_result = v["messages"].as_array().unwrap().iter().any(|m| m["role"] == "tool");
            let lines: Vec<Value> = if !has_tool_result {
                vec![json!({"message": {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "describe_table", "arguments": {"table": "users"}}}]}, "done": true})]
            } else {
                vec![json!({"message": {"content": "Here: "}, "done": false}), json!({"message": {"content": "SELECT 1"}, "done": true})]
            };
            lines.iter().map(|l| l.to_string() + "\n").collect::<String>()
        }),
    );
    let base = serve(router).await;
    let (emit, cap) = capture();
    let calls: Arc<Mutex<Vec<(String, Value)>>> = Default::default();
    let r = runner(calls.clone());
    let defs = definitions(&scope());
    providers::ollama(&provider(AiProviderKind::Ollama, &base), "sys".into(), &history(), &defs, &r, &emit).await.unwrap();
    assert_eq!(calls.lock().unwrap()[0], ("describe_table".to_string(), json!({"table": "users"})));
    assert_eq!(cap.text.lock().unwrap().trim(), "Here: SELECT 1");
}

#[tokio::test]
async fn ollama_model_without_tools_falls_back() {
    let router = Router::new().route(
        "/api/chat",
        post(|body: Bytes| async move {
            let v: Value = serde_json::from_slice(&body).unwrap();
            if v.get("tools").is_some() {
                return (axum::http::StatusCode::BAD_REQUEST, "{\"error\":\"registry.ollama.ai/library/x does not support tools\"}".to_string());
            }
            (axum::http::StatusCode::OK, json!({"message": {"content": "ok"}, "done": true}).to_string() + "\n")
        }),
    );
    let base = serve(router).await;
    let (emit, cap) = capture();
    let r = runner(Default::default());
    let defs = definitions(&scope());
    providers::ollama(&provider(AiProviderKind::Ollama, &base), "sys".into(), &history(), &defs, &r, &emit).await.unwrap();
    assert_eq!(cap.text.lock().unwrap().as_str(), "ok");
}

#[tokio::test]
async fn anthropic_sse_with_tool_use() {
    let log: Log = Default::default();
    let l = log.clone();
    let router = Router::new().route(
        "/v1/messages",
        post(move |headers: axum::http::HeaderMap, body: Bytes| {
            let l = l.clone();
            async move {
                assert_eq!(headers.get("x-api-key").unwrap(), "key");
                assert!(headers.get("anthropic-version").is_some());
                let v: Value = serde_json::from_slice(&body).unwrap();
                l.lock().unwrap().push(v.clone());
                let second = v["messages"].as_array().unwrap().len() > 1;
                let events: Vec<Value> = if !second {
                    vec![
                        json!({"type": "message_start", "message": {}}),
                        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Let me look. "}}),
                        json!({"type": "content_block_stop", "index": 0}),
                        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "tu_1", "name": "list_tables", "input": {}}}),
                        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"schema\": \"pub"}}),
                        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "lic\"}"}}),
                        json!({"type": "content_block_stop", "index": 1}),
                        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}),
                        json!({"type": "message_stop"}),
                    ]
                } else {
                    vec![
                        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Done."}}),
                        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
                    ]
                };
                let s: String = events.iter().map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap())).collect();
                ([("content-type", "text/event-stream")], s)
            }
        }),
    );
    let base = serve(router).await;
    let (emit, cap) = capture();
    let calls: Arc<Mutex<Vec<(String, Value)>>> = Default::default();
    let r = runner(calls.clone());
    let defs = definitions(&scope());
    providers::anthropic(&provider(AiProviderKind::Anthropic, &base), Some("key".into()), "sys".into(), &history(), &defs, &r, &emit).await.unwrap();
    assert_eq!(calls.lock().unwrap()[0], ("list_tables".to_string(), json!({"schema": "public"})));
    assert!(cap.text.lock().unwrap().contains("Done."));
    let reqs = log.lock().unwrap();
    assert_eq!(reqs[0]["system"], "sys");
    let second = reqs[1]["messages"].as_array().unwrap();
    assert_eq!(second[1]["role"], "assistant");
    assert_eq!(second[1]["content"][1]["type"], "tool_use");
    assert_eq!(second[1]["content"][1]["input"], json!({"schema": "public"}));
    assert_eq!(second[2]["content"][0]["type"], "tool_result");
    assert_eq!(second[2]["content"][0]["tool_use_id"], "tu_1");
}

#[test]
fn http_endpoints_must_be_https_unless_local() {
    use sqlighter_lib::ai::validate_provider;
    assert!(validate_provider(&provider(AiProviderKind::Openai, "https://api.openai.com/v1")).is_ok());
    assert!(validate_provider(&provider(AiProviderKind::Ollama, "http://127.0.0.1:11434")).is_ok());
    assert!(validate_provider(&provider(AiProviderKind::Ollama, "http://localhost:11434")).is_ok());
    assert!(validate_provider(&provider(AiProviderKind::Openai, "http://api.example.com/v1")).is_err());
    let mut lan = provider(AiProviderKind::Ollama, "http://192.168.1.20:11434");
    assert!(validate_provider(&lan).is_err());
    lan.allow_insecure_http = true;
    assert!(validate_provider(&lan).is_ok());
    assert!(validate_provider(&provider(AiProviderKind::Openai, "file:///etc/passwd")).is_err());
}

#[test]
fn tool_access_levels() {
    let mut s = scope();
    s.access = AiAccessLevel::None;
    assert!(definitions(&s).is_empty());
    s.access = AiAccessLevel::Schema;
    let names: Vec<&str> = definitions(&s).iter().map(|d| d.name).collect();
    assert_eq!(names, ["list_tables", "describe_table"]);
    s.access = AiAccessLevel::Read;
    assert!(definitions(&s).iter().any(|d| d.name == "run_query"));
    s.connection_id = None;
    assert!(definitions(&s).iter().any(|d| d.name == "list_connections"));
    assert!(definitions(&s).iter().all(|d| d.name == "list_connections" || d.schema["required"].as_array().unwrap().contains(&json!("connection"))));
}
