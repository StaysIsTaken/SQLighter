//! AI assistant: builds the context (dialect, schema, editor SQL), dispatches to the configured
//! provider and streams "ai-event"s to the UI.

pub mod claude_code;
pub mod providers;
pub mod tools;

use std::sync::Arc;

use serde::Serialize;
use tauri::{Emitter, State};

use crate::app_err;
use crate::error::{AppError, AppResult};
use crate::model::*;
use crate::state::AppState;

type S<'a> = State<'a, Arc<AppState>>;

/// Event sink for one chat request.
#[derive(Clone)]
pub struct Emit {
    sink: Arc<dyn Fn(ChatEventKind) + Send + Sync>,
}

/// Executes a tool call and returns its textual result (errors are returned as "ERROR: ...").
pub type ToolRunner = dyn Fn(String, serde_json::Value) -> futures::future::BoxFuture<'static, String> + Send + Sync;

impl Emit {
    pub fn new(sink: Arc<dyn Fn(ChatEventKind) + Send + Sync>) -> Self {
        Emit { sink }
    }
    fn tauri(app: tauri::AppHandle, request_id: String) -> Self {
        Emit::new(Arc::new(move |kind| {
            let _ = app.emit("ai-event", ChatEvent { request_id: request_id.clone(), kind });
        }))
    }
    pub fn send(&self, kind: ChatEventKind) {
        (self.sink)(kind)
    }
    pub fn text(&self, t: &str) {
        self.send(ChatEventKind::Text { text: t.to_string() });
    }
    pub fn tool(&self, name: &str, detail: &str) {
        self.send(ChatEventKind::Tool { name: name.to_string(), detail: detail.chars().take(300).collect() });
    }
    pub fn session(&self, id: &str) {
        self.send(ChatEventKind::Session { session_id: id.to_string() });
    }
}

/// Endpoints must use HTTPS; plain HTTP is only allowed for loopback (e.g. local Ollama) or when
/// the user explicitly opts in for a LAN host.
pub fn validate_provider(p: &AiProviderConfig) -> AppResult<()> {
    if p.kind == AiProviderKind::ClaudeCode {
        return Ok(());
    }
    let url = reqwest::Url::parse(p.base_url.trim()).map_err(|e| app_err!("{}: invalid URL ({e})", p.name))?;
    match url.scheme() {
        "https" => Ok(()),
        "http" => {
            let host = url.host_str().unwrap_or("");
            if crate::tls::is_loopback(host) || p.allow_insecure_http {
                Ok(())
            } else {
                Err(app_err!(
                    "{}: unencrypted http:// is only allowed for localhost. Use https:// or explicitly allow insecure HTTP for this provider.",
                    p.name
                ))
            }
        }
        s => Err(app_err!("{}: unsupported URL scheme '{s}'", p.name)),
    }
}

fn system_prompt(access: AiAccessLevel, dialect: Option<Dialect>, has_tools: bool) -> String {
    let mut s = String::from(
        "You are the SQL assistant built into SQLighter, a database client. Help the user write correct, efficient and safe SQL.\n\
         Rules:\n\
         - Put every SQL statement you propose into a fenced ```sql code block. The user can insert or run it from there.\n\
         - Use the exact table and column names from the schema and quote identifiers when the dialect requires it.\n\
         - You never execute data-modifying SQL yourself; the user reviews and runs statements.\n\
         - Warn clearly about destructive operations (DROP, TRUNCATE, DELETE/UPDATE without WHERE) and suggest a safer variant.\n\
         - Keep explanations short. Answer in the language the user writes in.\n",
    );
    if let Some(d) = dialect {
        s.push_str(&format!("- Target SQL dialect: {d:?}. Use syntax that works for this database.\n"));
    }
    if has_tools {
        s.push_str("- Use the available tools to inspect tables before writing SQL for tables you have not seen yet.\n");
    }
    if access == AiAccessLevel::Read {
        s.push_str("- run_query executes read-only statements only; use it sparingly (LIMIT your queries).\n");
    }
    s
}

#[tauri::command]
pub async fn ai_chat(state: S<'_>, request: ChatRequest) -> AppResult<()> {
    let st = state.inner().clone();
    let settings = st.store.settings();
    let provider = settings.ai_providers.iter().find(|p| p.id == request.provider_id).cloned().ok_or_else(|| app_err!("AI provider not found"))?;
    validate_provider(&provider)?;
    if request.messages.is_empty() {
        return Err(app_err!("empty conversation"));
    }
    let key = st.store.ai_api_key(&provider.id);
    let request_id = request.request_id.clone();
    let app = st.app.clone();
    let st2 = st.clone();
    let handle = tokio::spawn(async move {
        let emit = Emit::tauri(app.clone(), request.request_id.clone());
        let r = run_chat(st2.clone(), settings.ai_access, provider, key, &request, &emit).await;
        match r {
            Ok(()) => emit.send(ChatEventKind::Done),
            Err(e) => emit.send(ChatEventKind::Error { error: format!("{e:#}") }),
        }
        st2.tasks.lock().unwrap().remove(&format!("ai:{}", request.request_id));
    });
    st.tasks.lock().unwrap().insert(format!("ai:{request_id}"), handle.abort_handle());
    Ok(())
}

async fn run_chat(state: Arc<AppState>, access: AiAccessLevel, p: AiProviderConfig, key: Option<String>, req: &ChatRequest, emit: &Emit) -> anyhow::Result<()> {
    let conn_id = req.connection_id.clone().filter(|c| !c.is_empty());
    let (dialect, conn_name) = match &conn_id {
        Some(id) => match state.store.connection(id) {
            Some(c) => (Some(c.dialect()), c.name.clone()),
            None => (None, String::new()),
        },
        None => (None, String::new()),
    };
    let access = if conn_id.is_some() { access } else { AiAccessLevel::None };
    let scope = tools::Scope { connection_id: conn_id.clone(), access, allow_write: false, allow_open_editor: false, label: format!("AI chat ({})", p.name) };
    let has_tools = access != AiAccessLevel::None;
    let mut context = String::new();
    if let (Some(id), true) = (&conn_id, access != AiAccessLevel::None) {
        match tools::schema_context(&state, id, req.schema.as_deref()).await {
            Ok(s) => context.push_str(&format!("Connection: {conn_name}\n{s}\n")),
            Err(e) => context.push_str(&format!("(schema not available: {e:#})\n")),
        }
    } else if let Some(d) = dialect {
        context.push_str(&format!("Connection: {conn_name}, dialect {d:?} (schema access disabled in settings)\n"));
    }
    if let Some(sql) = req.editor_sql.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let sql: String = sql.chars().take(8000).collect();
        context.push_str(&format!("\nSQL currently in the user's editor:\n```sql\n{sql}\n```\n"));
    }
    let system = system_prompt(access, dialect, has_tools);
    if p.kind == AiProviderKind::ClaudeCode {
        return claude_code::chat(state, &p, system, context, req, scope, emit).await;
    }
    let system = if context.is_empty() { system } else { format!("{system}\n# Context\n{context}") };
    let history: Vec<ChatMessage> = req.messages.iter().filter(|m| m.role == "user" || m.role == "assistant").cloned().collect();
    let defs = tools::definitions(&scope);
    let runner: Box<ToolRunner> = Box::new(move |name: String, args: serde_json::Value| {
        let state = state.clone();
        let scope = scope.clone();
        Box::pin(async move {
            match tools::call(&state, &scope, &name, &args).await {
                Ok(s) => s,
                Err(e) => format!("ERROR: {e:#}"),
            }
        })
    });
    match p.kind {
        AiProviderKind::Ollama => providers::ollama(&p, system, &history, &defs, &runner, emit).await,
        AiProviderKind::Openai => providers::openai(&p, key, system, &history, &defs, &runner, emit).await,
        _ => providers::anthropic(&p, key, system, &history, &defs, &runner, emit).await,
    }
}

#[tauri::command]
pub fn ai_cancel(state: S<'_>, request_id: String) {
    if let Some(h) = state.tasks.lock().unwrap().remove(&format!("ai:{request_id}")) {
        h.abort();
        let _ = state.app.emit("ai-event", ChatEvent { request_id, kind: ChatEventKind::Done });
    }
}

#[tauri::command]
pub async fn ai_list_models(state: S<'_>, provider_id: String) -> AppResult<Vec<String>> {
    let p = state.store.settings().ai_providers.into_iter().find(|p| p.id == provider_id).ok_or_else(|| app_err!("provider not found"))?;
    validate_provider(&p)?;
    let key = state.store.ai_api_key(&p.id);
    providers::list_models(&p, key).await.map_err(AppError::from)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeInfo {
    pub path: String,
    pub version: String,
}

#[tauri::command]
pub async fn detect_claude(command: Option<String>) -> AppResult<ClaudeInfo> {
    let bin = claude_code::resolve(command.as_deref())?;
    let version = claude_code::version(&bin).await?;
    Ok(ClaudeInfo { path: bin.to_string_lossy().into_owned(), version })
}
