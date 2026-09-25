//! Chat providers with streaming and tool calling: Ollama, OpenAI-compatible APIs and the
//! Anthropic Messages API.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use serde_json::{json, Value};

use super::tools::ToolDef;
use super::{Emit, ToolRunner};
use crate::model::{AiProviderConfig, AiProviderKind, ChatMessage};

const MAX_TOOL_ROUNDS: usize = 10;

pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(300))
        .https_only(false) // validated per provider (http only for loopback or explicit opt-in)
        .user_agent(concat!("SQLighter/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

fn join(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.pointer("/error/message").or_else(|| v.get("error")).map(|e| e.as_str().map(String::from).unwrap_or_else(|| e.to_string())))
        .unwrap_or(body);
    bail!("{status}: {}", msg.chars().take(500).collect::<String>())
}

/// Splits a byte stream into lines (NDJSON / SSE).
struct Lines {
    buf: Vec<u8>,
}

impl Lines {
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = vec![];
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            out.push(String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string());
        }
        out
    }
}

fn tool_specs_openai(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.schema}}))
        .collect()
}

async fn run_tool(runner: &ToolRunner, emit: &Emit, name: &str, args: &Value) -> String {
    let detail = args.get("sql").or_else(|| args.get("table")).or_else(|| args.get("schema")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    emit.tool(name, &detail);
    runner(name.to_string(), args.clone()).await
}

// ------------------------------------------------------------------------------------------
// OpenAI-compatible (OpenAI, Azure-compatible gateways, LM Studio, vLLM, OpenRouter, ...)

pub async fn openai(p: &AiProviderConfig, key: Option<String>, system: String, history: &[ChatMessage], tool_defs: &[ToolDef], runner: &ToolRunner, emit: &Emit) -> Result<()> {
    let http = client()?;
    let mut messages: Vec<Value> = vec![json!({"role": "system", "content": system})];
    messages.extend(history.iter().map(|m| json!({"role": m.role, "content": m.content})));
    let tools = tool_specs_openai(tool_defs);
    for _ in 0..MAX_TOOL_ROUNDS {
        let mut body = json!({"model": p.model, "messages": messages, "stream": true});
        if let Some(t) = p.temperature {
            body["temperature"] = json!(t);
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        let mut req = http.post(join(&p.base_url, "chat/completions")).json(&body);
        if let Some(k) = key.as_deref().filter(|k| !k.is_empty()) {
            req = req.bearer_auth(k);
        }
        let resp = check(req.send().await?).await?;
        let mut stream = resp.bytes_stream();
        let mut lines = Lines { buf: vec![] };
        let mut text = String::new();
        // index -> (id, name, arguments)
        let mut calls: Vec<(String, String, String)> = vec![];
        'outer: while let Some(chunk) = stream.next().await {
            for line in lines.push(&chunk?) {
                let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
                if data == "[DONE]" {
                    break 'outer;
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                if let Some(e) = v.get("error") {
                    bail!("{}", e.get("message").and_then(|m| m.as_str()).unwrap_or(&e.to_string()));
                }
                let Some(choice) = v.pointer("/choices/0") else { continue };
                if let Some(t) = choice.pointer("/delta/content").and_then(|t| t.as_str()) {
                    text.push_str(t);
                    emit.text(t);
                }
                if let Some(tcs) = choice.pointer("/delta/tool_calls").and_then(|t| t.as_array()) {
                    for tc in tcs {
                        let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        while calls.len() <= idx {
                            calls.push(Default::default());
                        }
                        if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                            calls[idx].0 = id.to_string();
                        }
                        if let Some(n) = tc.pointer("/function/name").and_then(|x| x.as_str()) {
                            calls[idx].1.push_str(n);
                        }
                        if let Some(a) = tc.pointer("/function/arguments").and_then(|x| x.as_str()) {
                            calls[idx].2.push_str(a);
                        }
                    }
                }
            }
        }
        if calls.iter().all(|c| c.1.is_empty()) {
            return Ok(());
        }
        messages.push(json!({
            "role": "assistant",
            "content": if text.is_empty() { Value::Null } else { Value::String(text) },
            "tool_calls": calls.iter().map(|(id, name, args)| json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}})).collect::<Vec<_>>()
        }));
        for (id, name, args) in &calls {
            let a: Value = serde_json::from_str(if args.is_empty() { "{}" } else { args }).unwrap_or(json!({}));
            let result = run_tool(runner, emit, name, &a).await;
            messages.push(json!({"role": "tool", "tool_call_id": id, "content": result}));
        }
        emit.text("\n\n");
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------
// Ollama (/api/chat, NDJSON streaming)

pub async fn ollama(p: &AiProviderConfig, system: String, history: &[ChatMessage], tool_defs: &[ToolDef], runner: &ToolRunner, emit: &Emit) -> Result<()> {
    let http = client()?;
    let mut messages: Vec<Value> = vec![json!({"role": "system", "content": system})];
    messages.extend(history.iter().map(|m| json!({"role": m.role, "content": m.content})));
    let mut tools = tool_specs_openai(tool_defs);
    for _ in 0..MAX_TOOL_ROUNDS {
        let mut body = json!({"model": p.model, "messages": messages, "stream": true});
        if let Some(t) = p.temperature {
            body["options"] = json!({"temperature": t});
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        let resp = http.post(join(&p.base_url, "api/chat")).json(&body).send().await.map_err(|e| {
            if e.is_connect() {
                anyhow!("Ollama is not reachable at {}. Is it running? ({e})", p.base_url)
            } else {
                e.into()
            }
        })?;
        if resp.status() == reqwest::StatusCode::BAD_REQUEST && !tools.is_empty() {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("does not support tools") {
                // Model without tool support: continue with the schema in the system prompt only.
                tools.clear();
                continue;
            }
            bail!("400: {body}");
        }
        let resp = check(resp).await?;
        let mut stream = resp.bytes_stream();
        let mut lines = Lines { buf: vec![] };
        let mut text = String::new();
        let mut calls: Vec<Value> = vec![];
        while let Some(chunk) = stream.next().await {
            for line in lines.push(&chunk?) {
                if line.trim().is_empty() {
                    continue;
                }
                let v: Value = serde_json::from_str(&line).context("invalid response from Ollama")?;
                if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
                    bail!("{e}");
                }
                if let Some(t) = v.pointer("/message/content").and_then(|t| t.as_str()) {
                    if !t.is_empty() {
                        text.push_str(t);
                        emit.text(t);
                    }
                }
                if let Some(tc) = v.pointer("/message/tool_calls").and_then(|t| t.as_array()) {
                    calls.extend(tc.iter().cloned());
                }
            }
        }
        if calls.is_empty() {
            return Ok(());
        }
        messages.push(json!({"role": "assistant", "content": text, "tool_calls": calls}));
        for c in &calls {
            let name = c.pointer("/function/name").and_then(|n| n.as_str()).unwrap_or("");
            let args = c.pointer("/function/arguments").cloned().unwrap_or(json!({}));
            let args = if let Some(s) = args.as_str() { serde_json::from_str(s).unwrap_or(json!({})) } else { args };
            let result = run_tool(runner, emit, name, &args).await;
            messages.push(json!({"role": "tool", "content": result, "tool_name": name}));
        }
        emit.text("\n\n");
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------
// Anthropic Messages API

pub async fn anthropic(p: &AiProviderConfig, key: Option<String>, system: String, history: &[ChatMessage], tool_defs: &[ToolDef], runner: &ToolRunner, emit: &Emit) -> Result<()> {
    let key = key.filter(|k| !k.is_empty()).context("No API key configured for this provider (Settings → AI)")?;
    let http = client()?;
    let mut messages: Vec<Value> = history.iter().map(|m| json!({"role": m.role, "content": m.content})).collect();
    let tools: Vec<Value> = tool_defs.iter().map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema})).collect();
    for _ in 0..MAX_TOOL_ROUNDS {
        let mut body = json!({"model": p.model, "max_tokens": 8192, "system": system, "messages": messages, "stream": true});
        if let Some(t) = p.temperature {
            body["temperature"] = json!(t);
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        let resp = http
            .post(join(&p.base_url, "v1/messages"))
            .header("x-api-key", key.as_str())
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await?;
        let resp = check(resp).await?;
        let mut stream = resp.bytes_stream();
        let mut lines = Lines { buf: vec![] };
        // Content blocks of the assistant turn: text or tool_use (with streamed JSON input)
        let mut blocks: Vec<Value> = vec![];
        let mut partial: Vec<String> = vec![];
        let mut stop_reason = String::new();
        while let Some(chunk) = stream.next().await {
            for line in lines.push(&chunk?) {
                let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
                let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "content_block_start" => {
                        let b = v.get("content_block").cloned().unwrap_or(json!({}));
                        blocks.push(b);
                        partial.push(String::new());
                    }
                    "content_block_delta" => {
                        let i = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        let d = v.get("delta").cloned().unwrap_or(json!({}));
                        match d.get("type").and_then(|t| t.as_str()) {
                            Some("text_delta") => {
                                let t = d.get("text").and_then(|t| t.as_str()).unwrap_or("");
                                emit.text(t);
                                if let Some(b) = blocks.get_mut(i) {
                                    let cur = b.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                    b["text"] = json!(cur + t);
                                }
                            }
                            Some("input_json_delta") => {
                                if let Some(pj) = partial.get_mut(i) {
                                    pj.push_str(d.get("partial_json").and_then(|t| t.as_str()).unwrap_or(""));
                                }
                            }
                            _ => {}
                        }
                    }
                    "message_delta" => {
                        if let Some(r) = v.pointer("/delta/stop_reason").and_then(|r| r.as_str()) {
                            stop_reason = r.to_string();
                        }
                    }
                    "error" => bail!("{}", v.pointer("/error/message").and_then(|m| m.as_str()).unwrap_or("Anthropic API error")),
                    _ => {}
                }
            }
        }
        for (i, b) in blocks.iter_mut().enumerate() {
            if b.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                let raw = partial.get(i).cloned().unwrap_or_default();
                b["input"] = serde_json::from_str(if raw.is_empty() { "{}" } else { &raw }).unwrap_or(json!({}));
            }
        }
        if stop_reason != "tool_use" {
            return Ok(());
        }
        let uses: Vec<Value> = blocks.iter().filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use")).cloned().collect();
        // Thinking blocks etc. are passed back unchanged; empty text blocks are not allowed.
        let assistant: Vec<Value> = blocks.into_iter().filter(|b| !(b.get("type").and_then(|t| t.as_str()) == Some("text") && b.get("text").and_then(|t| t.as_str()).unwrap_or("").is_empty())).collect();
        messages.push(json!({"role": "assistant", "content": assistant}));
        let mut results = vec![];
        for u in uses {
            let name = u.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let input = u.get("input").cloned().unwrap_or(json!({}));
            let out = run_tool(runner, emit, name, &input).await;
            results.push(json!({"type": "tool_result", "tool_use_id": u.get("id").cloned().unwrap_or(Value::Null), "content": out, "is_error": out.starts_with("ERROR:")}));
        }
        messages.push(json!({"role": "user", "content": results}));
        emit.text("\n\n");
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------

pub async fn list_models(p: &AiProviderConfig, key: Option<String>) -> Result<Vec<String>> {
    let http = client()?;
    let mut out: Vec<String> = match p.kind {
        AiProviderKind::Ollama => {
            let v: Value = check(http.get(join(&p.base_url, "api/tags")).send().await?).await?.json().await?;
            v.get("models").and_then(|m| m.as_array()).map(|a| a.iter().filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from)).collect()).unwrap_or_default()
        }
        AiProviderKind::Openai => {
            let mut req = http.get(join(&p.base_url, "models"));
            if let Some(k) = key.as_deref().filter(|k| !k.is_empty()) {
                req = req.bearer_auth(k);
            }
            let v: Value = check(req.send().await?).await?.json().await?;
            v.get("data").and_then(|m| m.as_array()).map(|a| a.iter().filter_map(|m| m.get("id").and_then(|n| n.as_str()).map(String::from)).collect()).unwrap_or_default()
        }
        AiProviderKind::Anthropic => {
            let key = key.context("API key missing")?;
            let v: Value = check(http.get(join(&p.base_url, "v1/models")).header("x-api-key", key).header("anthropic-version", "2023-06-01").send().await?)
                .await?
                .json()
                .await?;
            v.get("data").and_then(|m| m.as_array()).map(|a| a.iter().filter_map(|m| m.get("id").and_then(|n| n.as_str()).map(String::from)).collect()).unwrap_or_default()
        }
        AiProviderKind::ClaudeCode => vec!["opus".into(), "sonnet".into(), "haiku".into()],
    };
    out.sort();
    Ok(out)
}
