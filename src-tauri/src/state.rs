//! Global application state shared by commands, the MCP server and AI agents.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tauri::{AppHandle, Emitter};
use tokio::sync::{oneshot, RwLock};

use crate::db::session::Session;
use crate::model::*;
use crate::ssh::PromptFn;
use crate::store::Store;

pub struct AppState {
    pub store: Store,
    pub app: AppHandle,
    pub sessions: RwLock<HashMap<String, Arc<Session>>>,
    /// Files the user picked in a native dialog during this session. Commands that read or write
    /// files only accept these paths, so a compromised web view cannot touch arbitrary files.
    approved_paths: Mutex<HashSet<PathBuf>>,
    host_key_waiters: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    approvals: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    pub tasks: Mutex<HashMap<String, tokio::task::AbortHandle>>,
    pub mcp: crate::mcp::McpState,
}

impl AppState {
    pub fn new(app: AppHandle, store: Store) -> Self {
        AppState {
            store,
            app,
            sessions: RwLock::new(HashMap::new()),
            approved_paths: Mutex::new(HashSet::new()),
            host_key_waiters: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::new()),
            mcp: crate::mcp::McpState::default(),
        }
    }

    // --- sessions ------------------------------------------------------------------------

    pub async fn session(&self, id: &str) -> Result<Arc<Session>> {
        if let Some(s) = self.sessions.read().await.get(id) {
            return Ok(s.clone());
        }
        self.connect(id, None).await
    }

    pub async fn connect(&self, id: &str, password: Option<String>) -> Result<Arc<Session>> {
        if let Some(s) = self.sessions.read().await.get(id) {
            return Ok(s.clone());
        }
        let cfg = self.store.connection(id).context("connection not found")?;
        let mut secrets = self.store.secrets_for(id);
        if let Some(pw) = password {
            secrets.password = Some(pw);
        }
        let auto_commit = self.store.settings().auto_commit;
        let (session, new_fp) = Session::open(cfg, secrets, self.host_key_prompt(), auto_commit).await?;
        if let Some(fp) = new_fp {
            self.store.update_connection(|c| c.ssh.host_key_fingerprint = Some(fp), id)?;
        }
        let s = Arc::new(session);
        self.sessions.write().await.insert(id.to_string(), s.clone());
        Ok(s)
    }

    pub async fn disconnect(&self, id: &str) {
        let s = self.sessions.write().await.remove(id);
        if let Some(s) = s {
            s.close().await;
        }
    }

    // --- interactive prompts ---------------------------------------------------------------

    /// Asks the user (via the UI) whether an SSH host key should be trusted.
    pub fn host_key_prompt(&self) -> PromptFn {
        let app = self.app.clone();
        Arc::new(move |p: HostKeyPrompt| {
            let app = app.clone();
            Box::pin(async move {
                let state = crate::state(&app);
                let (tx, rx) = oneshot::channel();
                state.host_key_waiters.lock().unwrap().insert(p.prompt_id.clone(), tx);
                let id = p.prompt_id.clone();
                if app.emit("ssh-hostkey", &p).is_err() {
                    return false;
                }
                let ok = tokio::time::timeout(Duration::from_secs(180), rx).await.ok().and_then(|r| r.ok()).unwrap_or(false);
                state.host_key_waiters.lock().unwrap().remove(&id);
                ok
            })
        })
    }

    pub fn answer_host_key(&self, prompt_id: &str, accept: bool) {
        if let Some(tx) = self.host_key_waiters.lock().unwrap().remove(prompt_id) {
            let _ = tx.send(accept);
        }
    }

    /// Asks the user to approve a statement requested by an external agent (MCP).
    pub async fn request_approval(&self, source: &str, connection_name: &str, sql: &str) -> bool {
        let req = ApprovalRequest {
            approval_id: uuid::Uuid::new_v4().to_string(),
            source: source.to_string(),
            connection_name: connection_name.to_string(),
            sql: sql.to_string(),
        };
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().unwrap().insert(req.approval_id.clone(), tx);
        if self.app.emit("approval-request", &req).is_err() {
            return false;
        }
        let ok = tokio::time::timeout(Duration::from_secs(300), rx).await.ok().and_then(|r| r.ok()).unwrap_or(false);
        self.approvals.lock().unwrap().remove(&req.approval_id);
        ok
    }

    pub fn answer_approval(&self, id: &str, approve: bool) {
        if let Some(tx) = self.approvals.lock().unwrap().remove(id) {
            let _ = tx.send(approve);
        }
    }

    // --- file access -----------------------------------------------------------------------

    pub fn approve_path(&self, p: &Path) {
        self.approved_paths.lock().unwrap().insert(p.to_path_buf());
    }

    pub fn check_path(&self, p: &str) -> Result<PathBuf> {
        let pb = PathBuf::from(p);
        if self.approved_paths.lock().unwrap().contains(&pb) {
            return Ok(pb);
        }
        bail!("File access denied: '{p}' was not selected in a file dialog")
    }

    pub fn emit_progress(&self, p: &ProgressEvent) {
        let _ = self.app.emit("progress", p);
    }
}
