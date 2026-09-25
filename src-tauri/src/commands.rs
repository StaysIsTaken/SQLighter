//! Tauri commands for settings, connections, queries, metadata and files.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

use crate::app_err;
use crate::db::{metadata, session::ExecSettings};
use crate::error::AppResult;
use crate::model::*;
use crate::sql;
use crate::state::AppState;

type S<'a> = State<'a, Arc<AppState>>;

// --- settings ----------------------------------------------------------------------------

#[tauri::command]
pub fn get_settings(state: S<'_>) -> SettingsView {
    state.store.settings_view()
}

#[tauri::command]
pub async fn save_settings(state: S<'_>, settings: Settings) -> AppResult<SettingsView> {
    for p in &settings.ai_providers {
        crate::ai::validate_provider(p)?;
    }
    let before = state.store.settings();
    state.store.save_settings(settings.clone())?;
    if before.mcp_enabled != settings.mcp_enabled || before.mcp_port != settings.mcp_port {
        crate::mcp::apply_settings(state.inner().clone()).await?;
    }
    Ok(state.store.settings_view())
}

#[tauri::command]
pub fn set_ai_api_key(state: S<'_>, provider_id: String, api_key: Option<String>) {
    state.store.set_ai_api_key(&provider_id, api_key.as_deref());
}

// --- connections -------------------------------------------------------------------------

#[tauri::command]
pub fn get_tree(state: S<'_>) -> ConnectionTree {
    state.store.tree()
}

#[tauri::command]
pub async fn save_connection(state: S<'_>, config: ConnectionConfig, secrets: Option<ConnectionSecrets>) -> AppResult<ConnectionView> {
    validate_connection(&config)?;
    let id = config.id.clone();
    let v = state.store.save_connection(config, secrets)?;
    // Settings changed -> reconnect lazily with the new configuration.
    if !id.is_empty() {
        state.disconnect(&id).await;
    }
    Ok(v)
}

fn validate_connection(c: &ConnectionConfig) -> AppResult<()> {
    if c.name.trim().is_empty() {
        return Err(app_err!("Name is required"));
    }
    if c.db_type == DbType::Sqlite {
        if c.file_path.as_deref().unwrap_or("").trim().is_empty() {
            return Err(app_err!("SQLite database file is required"));
        }
    } else if c.host.trim().is_empty() {
        return Err(app_err!("Host is required"));
    }
    if c.host.chars().any(|ch| ch.is_control() || ch == ' ') {
        return Err(app_err!("Invalid host name"));
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_connection(state: S<'_>, id: String) -> AppResult<()> {
    state.disconnect(&id).await;
    state.store.delete_connection(&id)?;
    Ok(())
}

#[tauri::command]
pub fn duplicate_connection(state: S<'_>, id: String) -> AppResult<ConnectionView> {
    let mut c = state.store.connection(&id).ok_or_else(|| app_err!("connection not found"))?;
    let secrets = state.store.secrets_for(&id);
    c.id = String::new();
    c.created_at = 0;
    c.name = format!("{} (copy)", c.name);
    Ok(state.store.save_connection(c, Some(secrets))?)
}

#[tauri::command]
pub fn save_folder(state: S<'_>, folder: Folder) -> AppResult<Folder> {
    if folder.name.trim().is_empty() {
        return Err(app_err!("Folder name is required"));
    }
    Ok(state.store.save_folder(folder)?)
}

#[tauri::command]
pub fn delete_folder(state: S<'_>, id: String) -> AppResult<()> {
    Ok(state.store.delete_folder(&id)?)
}

#[tauri::command]
pub fn move_item(state: S<'_>, kind: String, id: String, folder_id: Option<String>, order: Option<f64>) -> AppResult<()> {
    Ok(state.store.move_item(&kind, &id, folder_id, order)?)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub server_version: String,
    pub host_key_fingerprint: Option<String>,
}

/// Tests a (possibly unsaved) configuration. Missing secrets fall back to the stored ones.
#[tauri::command]
pub async fn test_connection(state: S<'_>, config: ConnectionConfig, secrets: Option<ConnectionSecrets>) -> AppResult<TestResult> {
    validate_connection(&config)?;
    let stored = state.store.secrets_for(&config.id);
    let s = secrets.unwrap_or_default();
    let merged = ConnectionSecrets {
        password: s.password.or(stored.password),
        ssh_password: s.ssh_password.or(stored.ssh_password),
        ssh_passphrase: s.ssh_passphrase.or(stored.ssh_passphrase),
    };
    let fut = crate::db::session::Session::open(config, merged, state.host_key_prompt(), true);
    let (session, fp) = tokio::time::timeout(Duration::from_secs(240), fut).await.map_err(|_| app_err!("connection test timed out"))??;
    let v = session.server_version.clone();
    session.close().await;
    Ok(TestResult { server_version: v, host_key_fingerprint: fp })
}

/// Fetches the server certificate for trust-on-first-use pinning (PostgreSQL family).
#[tauri::command]
pub async fn fetch_server_cert(state: S<'_>, config: ConnectionConfig, secrets: Option<ConnectionSecrets>) -> AppResult<CertInfo> {
    if config.dialect() != Dialect::Postgres {
        return Err(app_err!("Certificate pinning is available for PostgreSQL and CockroachDB"));
    }
    let port = if config.port == 0 { crate::db::session::default_port(config.db_type) } else { config.port };
    if config.ssh.enabled {
        let stored = state.store.secrets_for(&config.id);
        let s = secrets.unwrap_or_default();
        let merged = ConnectionSecrets { password: None, ssh_password: s.ssh_password.or(stored.ssh_password), ssh_passphrase: s.ssh_passphrase.or(stored.ssh_passphrase) };
        let t = crate::ssh::SshTunnel::open(&config.ssh, &merged, &config.host, port, state.host_key_prompt()).await?;
        let stream = t.stream().await?;
        let r = crate::tls::probe_postgres_cert(stream, &config.host, port).await;
        t.close().await;
        return Ok(r?);
    }
    let tcp = tokio::time::timeout(Duration::from_secs(15), tokio::net::TcpStream::connect((config.host.as_str(), port)))
        .await
        .map_err(|_| app_err!("connection timed out"))??;
    Ok(crate::tls::probe_postgres_cert(tcp, &config.host, port).await?)
}

#[tauri::command]
pub fn answer_host_key(state: S<'_>, prompt_id: String, accept: bool) {
    state.answer_host_key(&prompt_id, accept);
}

#[tauri::command]
pub fn answer_approval(state: S<'_>, approval_id: String, approve: bool) {
    state.answer_approval(&approval_id, approve);
}

// --- sessions ------------------------------------------------------------------------------

#[tauri::command]
pub async fn connect(state: S<'_>, id: String, password: Option<String>) -> AppResult<SchemaSummary> {
    let s = state.connect(&id, password).await?;
    summary(&s).await
}

async fn summary(s: &crate::db::session::Session) -> AppResult<SchemaSummary> {
    let d = s.dialect();
    let mut g = s.meta().await?;
    let c = g.as_mut().unwrap();
    let schemas = metadata::schemas(c, d).await?;
    let default_schema = metadata::default_schema(c, d).await.unwrap_or_default();
    Ok(SchemaSummary { dialect: d, server_version: s.server_version.clone(), default_schema, schemas })
}

#[tauri::command]
pub async fn disconnect(state: S<'_>, id: String) -> AppResult<()> {
    state.disconnect(&id).await;
    Ok(())
}

#[tauri::command]
pub async fn connected_ids(state: S<'_>) -> AppResult<Vec<String>> {
    Ok(state.sessions.read().await.keys().cloned().collect())
}

#[tauri::command]
pub async fn schema_summary(state: S<'_>, id: String) -> AppResult<SchemaSummary> {
    let s = state.session(&id).await?;
    summary(&s).await
}

#[tauri::command]
pub async fn execute(state: S<'_>, connection_id: String, sql: String, options: Option<ExecuteOptions>) -> AppResult<ExecuteResponse> {
    let s = state.session(&connection_id).await?;
    let st = state.store.settings();
    let es = ExecSettings {
        confirm_destructive: st.confirm_destructive,
        max_rows: st.max_rows,
        timeout: if st.query_timeout_sec > 0 { Some(Duration::from_secs(st.query_timeout_sec)) } else { None },
    };
    let opts = options.unwrap_or_default();
    let started = std::time::Instant::now();
    let res = s.execute(&sql, &opts, &es).await?;
    if res.needs_confirmation.is_none() {
        state.store.add_history(HistoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            connection_id: connection_id.clone(),
            sql: sql.trim().to_string(),
            at: chrono::Utc::now().timestamp_millis(),
            duration_ms: started.elapsed().as_millis() as u64,
            ok: res.results.iter().all(|r| r.error.is_none()),
        });
    }
    Ok(res)
}

#[tauri::command]
pub async fn cancel_query(state: S<'_>, connection_id: String) -> AppResult<()> {
    if let Some(s) = state.sessions.read().await.get(&connection_id).cloned() {
        s.cancel().await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn commit(state: S<'_>, connection_id: String) -> AppResult<()> {
    Ok(state.session(&connection_id).await?.commit().await?)
}

#[tauri::command]
pub async fn rollback(state: S<'_>, connection_id: String) -> AppResult<()> {
    Ok(state.session(&connection_id).await?.rollback().await?)
}

#[tauri::command]
pub async fn set_auto_commit(state: S<'_>, connection_id: String, on: bool) -> AppResult<()> {
    Ok(state.session(&connection_id).await?.set_auto_commit(on).await?)
}

// --- metadata ------------------------------------------------------------------------------

#[tauri::command]
pub async fn list_objects(state: S<'_>, connection_id: String, schema: String) -> AppResult<Vec<DbObject>> {
    let s = state.session(&connection_id).await?;
    let d = s.dialect();
    let mut g = s.meta().await?;
    Ok(metadata::objects(g.as_mut().unwrap(), d, &schema).await?)
}

#[tauri::command]
pub async fn describe_table(state: S<'_>, connection_id: String, schema: String, name: String, kind: Option<ObjectKind>) -> AppResult<TableInfo> {
    let s = state.session(&connection_id).await?;
    let d = s.dialect();
    let mut g = s.meta().await?;
    Ok(metadata::describe(g.as_mut().unwrap(), d, &schema, &name, kind.unwrap_or(ObjectKind::Table)).await?)
}

#[tauri::command]
pub async fn get_ddl(state: S<'_>, connection_id: String, schema: String, name: String, kind: ObjectKind) -> AppResult<String> {
    let s = state.session(&connection_id).await?;
    let d = s.dialect();
    let mut g = s.meta().await?;
    Ok(metadata::ddl(g.as_mut().unwrap(), d, &schema, &name, kind).await?)
}

#[tauri::command]
pub async fn table_data(state: S<'_>, request: TableDataRequest) -> AppResult<QueryResult> {
    let s = state.session(&request.connection_id).await?;
    let d = s.dialect();
    let mut base = format!("SELECT * FROM {}", sql::qualified(&request.table.schema, &request.table.name, d));
    if let Some(w) = request.where_clause.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
        // The filter is user-typed SQL, like in the editor. Refuse anything that is not a pure
        // expression so it cannot smuggle a second statement.
        let probe = format!("SELECT 1 WHERE {w}");
        if sql::split_statements(&probe, d).len() != 1 {
            return Err(app_err!("The filter must be a single expression"));
        }
        base.push_str(&format!(" WHERE {w}"));
    }
    let has_order = !request.order_by.is_empty();
    if has_order {
        let o: Vec<String> = request.order_by.iter().map(|o| format!("{} {}", sql::quote_ident(&o.column, d), if o.desc { "DESC" } else { "ASC" })).collect();
        base.push_str(&format!(" ORDER BY {}", o.join(", ")));
    }
    let limit = request.limit.clamp(1, 100_000);
    let q = sql::limit_query(&base, limit + 1, request.offset, d, has_order);
    let mut g = s.meta().await?;
    let c = g.as_mut().unwrap();
    let start = std::time::Instant::now();
    let mut r = c.run(&q, limit + 1).await?.into_iter().find(|r| r.is_result_set).ok_or_else(|| app_err!("no result"))?;
    r.truncated = r.rows.len() > limit;
    r.rows.truncate(limit);
    r.duration_ms = start.elapsed().as_millis() as u64;
    Ok(r)
}

#[tauri::command]
pub async fn apply_changes(state: S<'_>, connection_id: String, statements: Vec<String>) -> AppResult<u64> {
    let s = state.session(&connection_id).await?;
    let d = s.dialect();
    // Grid edits are generated by the UI as INSERT/UPDATE/DELETE only.
    for st in &statements {
        let kw = sql::leading_keyword(st);
        if !matches!(kw.as_str(), "INSERT" | "UPDATE" | "DELETE") || sql::split_statements(st, d).len() != 1 {
            return Err(app_err!("Unexpected statement in change set"));
        }
    }
    if s.cfg.production {
        let conn_name = s.cfg.name.clone();
        if !state.request_approval("Table editor", &conn_name, &statements.join(";\n")).await {
            return Err(app_err!("Changes were not approved"));
        }
    }
    Ok(s.apply_in_transaction(&statements).await?)
}

// --- history -------------------------------------------------------------------------------

#[tauri::command]
pub fn get_history(state: S<'_>, connection_id: Option<String>) -> Vec<HistoryEntry> {
    state.store.history(connection_id.as_deref())
}

#[tauri::command]
pub fn clear_history(state: S<'_>) -> AppResult<()> {
    Ok(state.store.clear_history()?)
}

// --- files ---------------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct FileFilter {
    name: String,
    extensions: Vec<String>,
}

#[tauri::command]
pub async fn pick_open_file(state: S<'_>, filters: Vec<FileFilter>) -> AppResult<Option<String>> {
    let mut d = state.app.dialog().file();
    for f in &filters {
        let exts: Vec<&str> = f.extensions.iter().map(|s| s.as_str()).collect();
        d = d.add_filter(&f.name, &exts);
    }
    let picked = d.blocking_pick_file();
    Ok(picked.and_then(|p| p.into_path().ok()).map(|p| {
        state.approve_path(&p);
        p.to_string_lossy().into_owned()
    }))
}

#[tauri::command]
pub async fn pick_save_file(state: S<'_>, default_name: String, filters: Vec<FileFilter>) -> AppResult<Option<String>> {
    let mut d = state.app.dialog().file().set_file_name(&default_name);
    for f in &filters {
        let exts: Vec<&str> = f.extensions.iter().map(|s| s.as_str()).collect();
        d = d.add_filter(&f.name, &exts);
    }
    let picked = d.blocking_save_file();
    Ok(picked.and_then(|p| p.into_path().ok()).map(|p| {
        state.approve_path(&p);
        p.to_string_lossy().into_owned()
    }))
}

#[tauri::command]
pub fn read_text_file(state: S<'_>, path: String) -> AppResult<String> {
    let p = state.check_path(&path)?;
    let meta = std::fs::metadata(&p)?;
    if meta.len() > 50 * 1024 * 1024 {
        return Err(app_err!("File is too large to open in the editor (> 50 MB). Use Restore / Execute script instead."));
    }
    Ok(std::fs::read_to_string(p)?)
}

#[tauri::command]
pub fn write_text_file(state: S<'_>, path: String, content: String) -> AppResult<()> {
    let p = state.check_path(&path)?;
    std::fs::write(p, content)?;
    Ok(())
}
