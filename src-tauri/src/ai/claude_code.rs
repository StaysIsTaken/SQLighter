//! Claude Code integration: runs the local `claude` CLI in headless mode (stream-json). The CLI
//! gets no built-in tools (no shell, no file access); it only sees SQLighter's MCP tools through
//! a short-lived token that is bound to the current connection and revoked afterwards.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::tools::{self, Scope};
use super::Emit;
use crate::model::{AiProviderConfig, ChatRequest};
use crate::state::AppState;

const DISALLOWED: &str = "Bash,Edit,Write,MultiEdit,NotebookEdit,WebFetch,WebSearch,Read,Glob,Grep,Task,Agent,TodoWrite";

pub fn resolve(command: Option<&str>) -> Result<PathBuf> {
    if let Some(c) = command.map(str::trim).filter(|c| !c.is_empty()) {
        let p = PathBuf::from(crate::ssh::expand_home(c));
        if p.exists() {
            return Ok(p);
        }
        return which::which(c).with_context(|| format!("Claude Code executable '{c}' not found"));
    }
    if let Ok(p) = which::which("claude") {
        return Ok(p);
    }
    // GUI apps often start without the user's shell PATH.
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
    let candidates = [
        home.join(".claude/local/claude"),
        home.join(".local/bin/claude"),
        home.join(".npm-global/bin/claude"),
        home.join(".volta/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
        PathBuf::from("/opt/homebrew/bin/claude"),
        home.join(".local/bin/claude.exe"),
        home.join("AppData/Local/Programs/claude/claude.exe"),
    ];
    candidates.into_iter().find(|p| p.exists()).context("Claude Code CLI not found. Install it (https://code.claude.com) or set its path in Settings → AI.")
}

pub async fn version(bin: &PathBuf) -> Result<String> {
    let out = command(bin).arg("--version").output().await?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn command(bin: &PathBuf) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(bin);
    // Make sure a node-based install finds `node` even when launched from a GUI.
    let mut paths: Vec<PathBuf> = bin.parent().map(|p| vec![p.to_path_buf()]).unwrap_or_default();
    paths.extend(["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin"].iter().map(PathBuf::from));
    if let Some(cur) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&cur));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        cmd.env("PATH", joined);
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

pub async fn chat(state: Arc<AppState>, p: &AiProviderConfig, system: String, context: String, req: &ChatRequest, scope: Scope, emit: &Emit) -> Result<()> {
    let bin = resolve(p.command.as_deref())?;
    if bin.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat")) {
        bail!("Please use the native Claude Code installer (claude.exe); .cmd shims cannot be started safely.");
    }
    let (url, token) = crate::mcp::ephemeral_token(state.clone(), scope.clone()).await?;
    let dir = state.store.dir().join("claude-code");
    std::fs::create_dir_all(&dir)?;
    let cfg_path = dir.join(format!("mcp-{}.json", uuid::Uuid::new_v4()));
    let mcp_cfg = json!({"mcpServers": {"sqlighter": {"type": "http", "url": url, "headers": {"Authorization": format!("Bearer {token}")}}}});
    crate::store::write_private(&cfg_path, serde_json::to_string(&mcp_cfg)?.as_bytes())?;

    let allowed: Vec<String> = tools::definitions(&scope).iter().map(|t| format!("mcp__sqlighter__{}", t.name)).collect();
    let mut cmd = command(&bin);
    cmd.current_dir(&dir)
        .arg("-p")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--include-partial-messages")
        .arg("--append-system-prompt")
        .arg(&system)
        .arg("--mcp-config")
        .arg(&cfg_path)
        .arg("--strict-mcp-config")
        .arg("--disallowedTools")
        .arg(DISALLOWED);
    if !allowed.is_empty() {
        cmd.arg("--allowedTools").arg(allowed.join(","));
    }
    if !p.model.trim().is_empty() {
        cmd.arg("--model").arg(p.model.trim());
    }
    let resume = req.session_id.clone().filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    if let Some(sid) = &resume {
        cmd.arg("--resume").arg(sid);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().with_context(|| format!("start {}", bin.display()))?;

    // The prompt goes through stdin (no command line length limits, not visible in `ps`).
    let last = req.messages.last().map(|m| m.content.clone()).unwrap_or_default();
    let prompt = if resume.is_some() {
        format!("{last}\n\n<sqlighter-context>\n{context}\n</sqlighter-context>")
    } else {
        let mut s = format!("<sqlighter-context>\n{context}\n</sqlighter-context>\n\n");
        if req.messages.len() > 1 {
            s.push_str("Previous conversation:\n");
            for m in &req.messages[..req.messages.len() - 1] {
                s.push_str(&format!("[{}]\n{}\n\n", m.role, m.content));
            }
        }
        s.push_str(&last);
        s
    };
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(prompt.as_bytes()).await?;
    drop(stdin);

    let stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = tokio::io::AsyncReadExt::read_to_string(&mut stderr, &mut s).await;
        s
    });
    let mut lines = BufReader::new(stdout).lines();
    let mut got_text = false;
    let mut result_error: Option<String> = None;
    while let Some(line) = lines.next_line().await? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "system" => {
                if v.get("subtype").and_then(|s| s.as_str()) == Some("init") {
                    if let Some(sid) = v.get("session_id").and_then(|s| s.as_str()) {
                        emit.session(sid);
                    }
                }
            }
            "stream_event" => {
                let ev = v.get("event").cloned().unwrap_or(json!({}));
                match ev.get("type").and_then(|t| t.as_str()) {
                    Some("content_block_delta") => {
                        if ev.pointer("/delta/type").and_then(|t| t.as_str()) == Some("text_delta") {
                            if let Some(t) = ev.pointer("/delta/text").and_then(|t| t.as_str()) {
                                got_text = true;
                                emit.text(t);
                            }
                        }
                    }
                    Some("content_block_start") => {
                        if ev.pointer("/content_block/type").and_then(|t| t.as_str()) == Some("tool_use") {
                            let name = ev.pointer("/content_block/name").and_then(|t| t.as_str()).unwrap_or("");
                            emit.tool(name.trim_start_matches("mcp__sqlighter__"), "");
                        }
                    }
                    Some("message_stop") => {
                        if got_text {
                            emit.text("\n\n");
                            got_text = false;
                        }
                    }
                    _ => {}
                }
            }
            "result" => {
                if let Some(sid) = v.get("session_id").and_then(|s| s.as_str()) {
                    emit.session(sid);
                }
                if v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false) {
                    result_error = Some(v.get("result").and_then(|r| r.as_str()).unwrap_or("Claude Code reported an error").to_string());
                }
            }
            _ => {}
        }
    }
    let status = child.wait().await?;
    let stderr = err_task.await.unwrap_or_default();
    let _ = std::fs::remove_file(&cfg_path);
    crate::mcp::revoke_token(&state, &token);
    if let Some(e) = result_error {
        bail!(e);
    }
    if !status.success() {
        let msg = stderr.trim();
        if msg.contains("login") || msg.contains("auth") {
            bail!("Claude Code is not logged in. Run `claude` once in a terminal to sign in.\n{msg}");
        }
        bail!("Claude Code exited with {status}: {}", msg.chars().take(800).collect::<String>());
    }
    Ok(())
}
