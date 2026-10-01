//! Chat providers with streaming and tool calling: Ollama, OpenAI-compatible APIs and the
//! Anthropic Messages API.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use serde_json::{json, Value};

use super::tools::ToolDef;
use super::{Emit, ToolRunner};
use crate::model::{AiProviderConfig, AiProviderKind, ChatMessage};

// Exploring an unknown schema (search columns, describe, relationships) takes several rounds.
const MAX_TOOL_ROUNDS: usize = 20;

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

/// Upper bound for the context window SQLighter requests from Ollama (memory use grows with it).
const OLLAMA_MAX_CTX: u64 = 32_768;
/// Room left for the model's answer.
const OLLAMA_ANSWER_TOKENS: u64 = 2_048;

/// Rough token estimate (≈ 3 characters per token for SQL / mixed-language text).
pub fn estimate_tokens(v: &Value) -> u64 {
    (v.to_string().len() as u64).div_ceil(3)
}

/// Context window to request: large enough for the prompt plus an answer, at least 8k (Ollama's
/// default of 2–4k silently cuts off the beginning of the prompt), at most what the model supports.
pub fn ollama_num_ctx(prompt_tokens: u64, model_max: Option<u64>) -> u64 {
    let cap = model_max.unwrap_or(OLLAMA_MAX_CTX).min(OLLAMA_MAX_CTX).max(2_048);
    (prompt_tokens + OLLAMA_ANSWER_TOKENS).div_ceil(2_048).saturating_mul(2_048).clamp(8_192.min(cap), cap)
}

/// The model's maximum context length from `/api/show` (cached per base URL + model).
async fn ollama_model_max(http: &reqwest::Client, p: &AiProviderConfig) -> Option<u64> {
    static CACHE: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Option<u64>>>> = std::sync::LazyLock::new(Default::default);
    let key = format!("{}|{}", p.base_url, p.model);
    if let Some(v) = CACHE.lock().unwrap().get(&key) {
        return *v;
    }
    let v: Option<u64> = async {
        let r = http.post(join(&p.base_url, "api/show")).json(&json!({"model": p.model})).send().await.ok()?;
        let v: Value = r.error_for_status().ok()?.json().await.ok()?;
        v.get("model_info")?.as_object()?.iter().find(|(k, _)| k.ends_with(".context_length")).and_then(|(_, v)| v.as_u64())
    }
    .await;
    CACHE.lock().unwrap().insert(key, v);
    v
}

pub async fn ollama(p: &AiProviderConfig, system: String, history: &[ChatMessage], tool_defs: &[ToolDef], runner: &ToolRunner, emit: &Emit) -> Result<()> {
    let http = client()?;
    let mut messages: Vec<Value> = vec![json!({"role": "system", "content": system})];
    messages.extend(history.iter().map(|m| json!({"role": m.role, "content": m.content})));
    let mut tools = tool_specs_openai(tool_defs);
    let model_max = ollama_model_max(&http, p).await;
    let mut warned = false;
    for _ in 0..MAX_TOOL_ROUNDS {
        let prompt_tokens = estimate_tokens(&json!(messages)) + estimate_tokens(&json!(tools));
        let num_ctx = ollama_num_ctx(prompt_tokens, model_max);
        if prompt_tokens + 512 > num_ctx && !warned {
            // Ollama would drop the beginning of the conversation (instructions, question).
            emit.notice("context-overflow");
            warned = true;
        }
        let mut body = json!({"model": p.model, "messages": messages, "stream": true, "options": {"num_ctx": num_ctx}});
        if let Some(t) = p.temperature {
            body["options"]["temperature"] = json!(t);
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

/// A model offered by a provider. `name` is a display name where the API provides one.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Models that cannot be used for chat (embeddings, audio, images, moderation, legacy completions).
fn is_chat_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    !["embed", "tts", "whisper", "dall-e", "moderation", "davinci", "babbage", "transcribe", "gpt-image", "-realtime", "-audio", "-search", "rerank"]
        .iter()
        .any(|x| id.contains(x))
}

pub async fn list_models(p: &AiProviderConfig, key: Option<String>) -> Result<Vec<ModelInfo>> {
    let http = client()?;
    let ids = |v: &Value, arr: &str, field: &str| -> Vec<ModelInfo> {
        v.get(arr)
            .and_then(|m| m.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        let id = m.get(field)?.as_str()?.to_string();
                        let name = m.get("display_name").and_then(|n| n.as_str()).map(String::from).filter(|n| *n != id);
                        Some(ModelInfo { id, name })
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out: Vec<ModelInfo> = match p.kind {
        AiProviderKind::Ollama => {
            let v: Value = check(http.get(join(&p.base_url, "api/tags")).send().await?).await?.json().await?;
            ids(&v, "models", "name")
        }
        AiProviderKind::Openai => {
            let mut req = http.get(join(&p.base_url, "models"));
            if let Some(k) = key.as_deref().filter(|k| !k.is_empty()) {
                req = req.bearer_auth(k);
            }
            let v: Value = check(req.send().await?).await?.json().await?;
            ids(&v, "data", "id")
        }
        AiProviderKind::Anthropic => {
            let key = key.filter(|k| !k.is_empty()).context("Enter the API key to load the models.")?;
            let v: Value = check(http.get(join(&p.base_url, "v1/models?limit=100")).header("x-api-key", key).header("anthropic-version", "2023-06-01").send().await?)
                .await?
                .json()
                .await?;
            // Newest first, as returned by the API.
            return Ok(ids(&v, "data", "id"));
        }
        AiProviderKind::ClaudeCode => {
            return Ok(["opus", "sonnet", "haiku"].iter().map(|m| ModelInfo { id: m.to_string(), name: None }).collect());
        }
    };
    out.retain(|m| is_chat_model(&m.id));
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup();
    Ok(out)
}
