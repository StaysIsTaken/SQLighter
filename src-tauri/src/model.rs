//! Data model shared with the frontend (see src/shared/types.ts). Field names are camelCase on
//! the wire.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbType {
    Postgres,
    Cockroach,
    Mysql,
    Mariadb,
    Sqlite,
    Mssql,
    Oracle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    Postgres,
    Mysql,
    Sqlite,
    Mssql,
    Oracle,
}

impl DbType {
    pub fn dialect(self) -> Dialect {
        match self {
            DbType::Postgres | DbType::Cockroach => Dialect::Postgres,
            DbType::Mysql | DbType::Mariadb => Dialect::Mysql,
            DbType::Sqlite => Dialect::Sqlite,
            DbType::Mssql => Dialect::Mssql,
            DbType::Oracle => Dialect::Oracle,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            DbType::Postgres => "PostgreSQL",
            DbType::Cockroach => "CockroachDB",
            DbType::Mysql => "MySQL",
            DbType::Mariadb => "MariaDB",
            DbType::Sqlite => "SQLite",
            DbType::Mssql => "SQL Server",
            DbType::Oracle => "Oracle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    Disabled,
    #[default]
    VerifyFull,
    VerifyCa,
    Pinned,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TlsConfig {
    pub mode: TlsMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_cert: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_fingerprint: Option<String>,
    #[serde(default)]
    pub allow_insecure: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SshAuth {
    #[default]
    Password,
    Key,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfig {
    pub enabled: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub auth: SshAuth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_key_fingerprint: Option<String>,
}

fn default_ssh_port() -> u16 {
    22
}

impl Default for SshConfig {
    fn default() -> Self {
        SshConfig { enabled: false, host: String::new(), port: 22, user: String::new(), auth: SshAuth::Password, key_file: None, host_key_fingerprint: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionConfig {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub db_type: DbType,
    pub folder_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub user: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_name: Option<String>,
    #[serde(default = "yes")]
    pub save_password: bool,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub production: bool,
    #[serde(default)]
    pub tls: TlsConfig,
    #[serde(default)]
    pub ssh: SshConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<f64>,
    #[serde(default)]
    pub created_at: i64,
}

fn yes() -> bool {
    true
}

impl ConnectionConfig {
    pub fn dialect(&self) -> Dialect {
        self.db_type.dialect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSecrets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_passphrase: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionView {
    #[serde(flatten)]
    pub config: ConnectionConfig,
    pub has_password: bool,
    pub has_ssh_password: bool,
    pub has_ssh_passphrase: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<f64>,
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionTree {
    pub folders: Vec<Folder>,
    pub connections: Vec<ConnectionView>,
}

// ------------------------------------------------------------------------------------------
// Results & metadata

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMeta {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub type_name: Option<String>,
}

pub type Row = Vec<Value>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub statement: String,
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Row>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected_rows: Option<u64>,
    pub truncated: bool,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub is_result_set: bool,
}

impl QueryResult {
    pub fn command(statement: &str, affected: Option<u64>) -> Self {
        QueryResult {
            statement: statement.to_string(),
            columns: vec![],
            rows: vec![],
            affected_rows: affected,
            truncated: false,
            duration_ms: 0,
            error: None,
            is_result_set: false,
        }
    }
    pub fn rows(statement: &str, columns: Vec<ColumnMeta>, rows: Vec<Row>, truncated: bool) -> Self {
        QueryResult { statement: statement.to_string(), columns, rows, affected_rows: None, truncated, duration_ms: 0, error: None, is_result_set: true }
    }
    pub fn failed(statement: &str, error: String) -> Self {
        QueryResult { error: Some(error), ..QueryResult::command(statement, None) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteOptions {
    #[serde(default)]
    pub max_rows: Option<usize>,
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NeedsConfirmation {
    pub reason: String,
    pub statements: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteResponse {
    pub results: Vec<QueryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_confirmation: Option<NeedsConfirmation>,
    pub in_transaction: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,
    Function,
    Procedure,
    Sequence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DbObject {
    pub name: String,
    pub kind: ObjectKind,
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    #[serde(default)]
    pub default_value: Option<String>,
    pub is_primary_key: bool,
    #[serde(default)]
    pub auto_increment: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default)]
    pub max_length: Option<i64>,
    #[serde(default)]
    pub precision: Option<i64>,
    #[serde(default)]
    pub scale: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    #[serde(default)]
    pub primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_update: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
    pub columns: Vec<ColumnInfo>,
    pub primary_key: Vec<String>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default)]
    pub row_estimate: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaSummary {
    pub dialect: Dialect,
    pub server_version: String,
    pub default_schema: String,
    pub schemas: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableRef {
    pub schema: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderBy {
    pub column: String,
    pub desc: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDataRequest {
    pub connection_id: String,
    pub table: TableRef,
    #[serde(default)]
    pub where_clause: Option<String>,
    #[serde(default)]
    pub order_by: Vec<OrderBy>,
    pub limit: usize,
    pub offset: usize,
}

// ------------------------------------------------------------------------------------------
// Import / export / backup

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Csv,
    Tsv,
    Json,
    Xml,
    Xlsx,
    Sql,
    Markdown,
    Html,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportFormat {
    Csv,
    Tsv,
    Json,
    Xml,
    Xlsx,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExportSource {
    #[serde(rename_all = "camelCase")]
    Rows { columns: Vec<ColumnMeta>, rows: Vec<Row>, table_name: Option<String>, dialect: Option<Dialect> },
    #[serde(rename_all = "camelCase")]
    Query { connection_id: String, sql: String, table_name: Option<String> },
    #[serde(rename_all = "camelCase")]
    Table { connection_id: String, table: TableRef },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub file_path: String,
    #[serde(default)]
    pub delimiter: Option<String>,
    #[serde(default = "yes")]
    pub include_header: bool,
    #[serde(default)]
    pub null_text: Option<String>,
    #[serde(default)]
    pub batch_size: Option<usize>,
    #[serde(default)]
    pub pretty_json: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    pub total_rows: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheets: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSourceOptions {
    pub format: ImportFormat,
    pub file_path: String,
    #[serde(default)]
    pub delimiter: Option<String>,
    #[serde(default = "yes")]
    pub has_header: bool,
    #[serde(default)]
    pub sheet: Option<String>,
    #[serde(default)]
    pub xml_row_tag: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    #[serde(flatten)]
    pub source: ImportSourceOptions,
    pub connection_id: String,
    pub table: TableRef,
    pub create_table: bool,
    pub truncate: bool,
    /// source column -> target column ("" = skip)
    pub mapping: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub batch_size: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferOptions {
    pub source_connection_id: String,
    pub source_schema: String,
    pub tables: Vec<String>,
    pub target_connection_id: String,
    pub target_schema: String,
    pub create_tables: bool,
    pub truncate: bool,
    pub batch_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackupMethod {
    Sql,
    Native,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupOptions {
    pub connection_id: String,
    pub file_path: String,
    pub method: BackupMethod,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub tables: Option<Vec<String>>,
    pub include_data: bool,
    pub include_ddl: bool,
    pub drop_statements: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOptions {
    pub connection_id: String,
    pub file_path: String,
    pub stop_on_error: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    pub task_id: String,
    pub label: String,
    pub done: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub finished: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ------------------------------------------------------------------------------------------
// AI & settings

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AiProviderKind {
    Ollama,
    Openai,
    Anthropic,
    ClaudeCode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProviderConfig {
    pub id: String,
    pub name: String,
    pub kind: AiProviderKind,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub allow_insecure_http: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProviderView {
    #[serde(flatten)]
    pub config: AiProviderConfig,
    pub has_api_key: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AiAccessLevel {
    None,
    #[default]
    Schema,
    Read,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub request_id: String,
    pub provider_id: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub editor_sql: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChatEventKind {
    Text { text: String },
    Tool { name: String, detail: String },
    Session { session_id: String },
    Done,
    Error { error: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEvent {
    pub request_id: String,
    #[serde(flatten)]
    pub kind: ChatEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub theme: String,
    pub language: String,
    pub editor_font_size: u32,
    pub max_rows: usize,
    pub confirm_destructive: bool,
    pub auto_commit: bool,
    pub ai_access: AiAccessLevel,
    pub ai_providers: Vec<AiProviderConfig>,
    pub ai_default_provider_id: Option<String>,
    pub mcp_enabled: bool,
    pub mcp_port: u16,
    pub mcp_allow_write: bool,
    pub query_timeout_sec: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "system".into(),
            language: "system".into(),
            editor_font_size: 13,
            max_rows: 1000,
            confirm_destructive: true,
            auto_commit: true,
            ai_access: AiAccessLevel::Schema,
            ai_providers: vec![
                AiProviderConfig {
                    id: "ollama".into(),
                    name: "Ollama (local)".into(),
                    kind: AiProviderKind::Ollama,
                    base_url: "http://127.0.0.1:11434".into(),
                    model: "qwen2.5-coder:7b".into(),
                    allow_insecure_http: false,
                    command: None,
                    temperature: Some(0.2),
                },
                AiProviderConfig {
                    id: "openai".into(),
                    name: "OpenAI".into(),
                    kind: AiProviderKind::Openai,
                    base_url: "https://api.openai.com/v1".into(),
                    model: "gpt-4.1-mini".into(),
                    allow_insecure_http: false,
                    command: None,
                    temperature: Some(0.2),
                },
                AiProviderConfig {
                    id: "anthropic".into(),
                    name: "Claude (Anthropic API)".into(),
                    kind: AiProviderKind::Anthropic,
                    base_url: "https://api.anthropic.com".into(),
                    model: "claude-sonnet-5".into(),
                    allow_insecure_http: false,
                    command: None,
                    temperature: Some(0.2),
                },
                AiProviderConfig {
                    id: "claude-code".into(),
                    name: "Claude Code".into(),
                    kind: AiProviderKind::ClaudeCode,
                    base_url: String::new(),
                    model: String::new(),
                    allow_insecure_http: false,
                    command: None,
                    temperature: None,
                },
            ],
            ai_default_provider_id: Some("ollama".into()),
            mcp_enabled: false,
            mcp_port: 47821,
            mcp_allow_write: false,
            query_timeout_sec: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecureStorageInfo {
    pub available: bool,
    pub backend: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub theme: String,
    pub language: String,
    pub editor_font_size: u32,
    pub max_rows: usize,
    pub confirm_destructive: bool,
    pub auto_commit: bool,
    pub ai_access: AiAccessLevel,
    pub ai_providers: Vec<AiProviderView>,
    pub ai_default_provider_id: Option<String>,
    pub mcp_enabled: bool,
    pub mcp_port: u16,
    pub mcp_allow_write: bool,
    pub query_timeout_sec: u64,
    pub secure_storage: SecureStorageInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    pub connection_id: String,
    pub sql: String,
    pub at: i64,
    pub duration_ms: u64,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyPrompt {
    pub prompt_id: String,
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub algorithm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CertInfo {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub subject: String,
    pub issuer: String,
    pub valid_from: String,
    pub valid_to: String,
    pub pem: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub approval_id: String,
    pub source: String,
    pub connection_name: String,
    pub sql: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInfo {
    pub enabled: bool,
    pub running: bool,
    pub url: String,
    pub token: String,
    pub bridge_path: String,
    pub claude_desktop_config: String,
    pub claude_code_command: String,
}
