// Types shared between the main process, the preload bridge and the renderer.

export type DbType = 'postgres' | 'cockroach' | 'mysql' | 'mariadb' | 'sqlite' | 'mssql' | 'oracle'

export type Dialect = 'postgres' | 'mysql' | 'sqlite' | 'mssql' | 'oracle'

export const DB_TYPES: { type: DbType; label: string; dialect: Dialect; defaultPort: number; file?: boolean }[] = [
  { type: 'postgres', label: 'PostgreSQL', dialect: 'postgres', defaultPort: 5432 },
  { type: 'mysql', label: 'MySQL', dialect: 'mysql', defaultPort: 3306 },
  { type: 'mariadb', label: 'MariaDB', dialect: 'mysql', defaultPort: 3306 },
  { type: 'sqlite', label: 'SQLite', dialect: 'sqlite', defaultPort: 0, file: true },
  { type: 'mssql', label: 'SQL Server', dialect: 'mssql', defaultPort: 1433 },
  { type: 'oracle', label: 'Oracle', dialect: 'oracle', defaultPort: 1521 },
  { type: 'cockroach', label: 'CockroachDB', dialect: 'postgres', defaultPort: 26257 }
]

export function dialectOf(type: DbType): Dialect {
  return DB_TYPES.find((d) => d.type === type)?.dialect ?? 'postgres'
}

/**
 * TLS modes. There is deliberately no "encrypt but don't verify" mode: it gives
 * a false sense of security because it is trivially intercepted (MITM).
 *  - disabled:    plain text. Only allowed without warning for loopback hosts or inside an SSH tunnel.
 *  - verify-full: certificate chain AND host name are verified (default).
 *  - verify-ca:   certificate chain is verified, host name is not.
 *  - pinned:      exactly one certificate (SHA-256 fingerprint) is trusted - for self-signed servers.
 */
export type TlsMode = 'disabled' | 'verify-full' | 'verify-ca' | 'pinned'

export interface TlsConfig {
  mode: TlsMode
  caFile?: string
  certFile?: string
  keyFile?: string
  /** PEM of the pinned server certificate (mode "pinned"). */
  pinnedCert?: string
  /** SHA-256 fingerprint (AA:BB:...) of the pinned certificate. */
  pinnedFingerprint?: string
  /** User explicitly acknowledged an unencrypted connection to a non-local host. */
  allowInsecure?: boolean
}

export type SshAuth = 'password' | 'key' | 'agent'

export interface SshConfig {
  enabled: boolean
  host: string
  port: number
  user: string
  auth: SshAuth
  keyFile?: string
  /** Trusted host key fingerprint "SHA256:base64". Unknown keys must be confirmed by the user. */
  hostKeyFingerprint?: string
}

export interface ConnectionConfig {
  id: string
  name: string
  type: DbType
  folderId: string | null
  color?: string
  host: string
  port: number
  database: string
  user: string
  /** SQLite database file. */
  filePath?: string
  /** Oracle service name. */
  serviceName?: string
  /** SQL Server instance name. */
  instanceName?: string
  savePassword: boolean
  readOnly: boolean
  /** Production connections ask for confirmation before any data-modifying statement. */
  production: boolean
  tls: TlsConfig
  ssh: SshConfig
  order?: number
  createdAt: number
}

/** Secrets never leave the main process in clear text except for what the user types. */
export interface ConnectionSecrets {
  password?: string
  sshPassword?: string
  sshPassphrase?: string
}

export interface ConnectionView extends ConnectionConfig {
  hasPassword: boolean
  hasSshPassword: boolean
  hasSshPassphrase: boolean
}

export interface Folder {
  id: string
  name: string
  parentId: string | null
  order?: number
  collapsed?: boolean
}

export interface ConnectionTree {
  folders: Folder[]
  connections: ConnectionView[]
}

// ---------------------------------------------------------------------------------------------
// Query results & metadata

export type CellValue = string | number | boolean | null

export interface ColumnMeta {
  name: string
  type?: string
}

export interface QueryResult {
  statement: string
  columns: ColumnMeta[]
  rows: CellValue[][]
  /** Rows affected by INSERT/UPDATE/DELETE. */
  affectedRows?: number
  /** More rows were available than returned. */
  truncated: boolean
  durationMs: number
  error?: string
  /** Statement returns a result set (vs. a command). */
  isResultSet: boolean
}

export interface ExecuteOptions {
  maxRows?: number
  /** User already confirmed that this may modify data. */
  confirmed?: boolean
}

export interface ExecuteResponse {
  results: QueryResult[]
  /** Execution stopped because the statement needs confirmation (production / destructive). */
  needsConfirmation?: { reason: string; statements: string[] }
  inTransaction: boolean
}

export type ObjectKind = 'table' | 'view' | 'materialized-view' | 'function' | 'procedure' | 'sequence'

export interface DbObject {
  name: string
  kind: ObjectKind
  schema: string
  comment?: string
}

export interface ColumnInfo {
  name: string
  dataType: string
  nullable: boolean
  defaultValue?: string | null
  isPrimaryKey: boolean
  autoIncrement?: boolean
  comment?: string
  maxLength?: number | null
  precision?: number | null
  scale?: number | null
}

export interface IndexInfo {
  name: string
  columns: string[]
  unique: boolean
  primary?: boolean
}

export interface ForeignKeyInfo {
  name: string
  columns: string[]
  refSchema: string
  refTable: string
  refColumns: string[]
  onDelete?: string
  onUpdate?: string
}

export interface TableInfo {
  schema: string
  name: string
  kind: ObjectKind
  columns: ColumnInfo[]
  primaryKey: string[]
  indexes: IndexInfo[]
  foreignKeys: ForeignKeyInfo[]
  comment?: string
  rowEstimate?: number | null
}

export interface SchemaSummary {
  dialect: Dialect
  serverVersion: string
  defaultSchema: string
  schemas: string[]
}

export interface TableRef {
  schema: string
  name: string
}

// ---------------------------------------------------------------------------------------------
// Table data editing

export interface RowChange {
  kind: 'update' | 'insert' | 'delete'
  /** Original values (update/delete) keyed by column name. */
  original?: Record<string, CellValue>
  /** New values (update/insert) keyed by column name. */
  values?: Record<string, CellValue>
}

export interface TableDataRequest {
  connectionId: string
  table: TableRef
  whereClause?: string
  orderBy?: { column: string; desc: boolean }[]
  limit: number
  offset: number
}

// ---------------------------------------------------------------------------------------------
// Import / export / backup / transfer

export type ExportFormat = 'csv' | 'tsv' | 'json' | 'xml' | 'xlsx' | 'sql' | 'markdown' | 'html'
export type ImportFormat = 'csv' | 'tsv' | 'json' | 'xml' | 'xlsx'

export type ExportSource =
  | { kind: 'rows'; columns: ColumnMeta[]; rows: CellValue[][]; tableName?: string; dialect?: Dialect }
  | { kind: 'query'; connectionId: string; sql: string; tableName?: string }
  | { kind: 'table'; connectionId: string; table: TableRef }

export interface ExportOptions {
  format: ExportFormat
  filePath: string
  delimiter?: string
  includeHeader?: boolean
  nullText?: string
  /** For SQL export: rows per INSERT statement. */
  batchSize?: number
  prettyJson?: boolean
}

export interface ImportPreview {
  columns: string[]
  rows: CellValue[][]
  totalRows: number
  sheets?: string[]
}

export interface ImportSourceOptions {
  format: ImportFormat
  filePath: string
  delimiter?: string
  hasHeader?: boolean
  sheet?: string
  xmlRowTag?: string
}

export interface ImportOptions {
  format: ImportFormat
  filePath: string
  connectionId: string
  table: TableRef
  createTable: boolean
  truncate: boolean
  /** source column -> target column ('' = skip). */
  mapping: Record<string, string>
  delimiter?: string
  hasHeader?: boolean
  sheet?: string
  /** XML: element name of a row, auto-detected when empty. */
  xmlRowTag?: string
  batchSize?: number
}

export interface TransferOptions {
  sourceConnectionId: string
  sourceSchema: string
  tables: string[]
  targetConnectionId: string
  targetSchema: string
  createTables: boolean
  truncate: boolean
  batchSize: number
}

export interface BackupOptions {
  connectionId: string
  filePath: string
  /** 'sql' = portable logical dump written by SQLighter; 'native' = pg_dump/mysqldump/sqlite backup API. */
  method: 'sql' | 'native'
  schema?: string
  tables?: string[]
  includeData: boolean
  includeDdl: boolean
  dropStatements: boolean
}

export interface RestoreOptions {
  connectionId: string
  filePath: string
  stopOnError: boolean
}

export interface ProgressEvent {
  taskId: string
  label: string
  done: number
  total?: number
  finished?: boolean
  error?: string
  message?: string
}

// ---------------------------------------------------------------------------------------------
// AI

export type AiProviderKind = 'ollama' | 'openai' | 'anthropic' | 'claude-code'

export interface AiProviderConfig {
  id: string
  name: string
  kind: AiProviderKind
  baseUrl: string
  model: string
  /** Allow http:// to a non-loopback host (e.g. Ollama in the LAN). Off by default. */
  allowInsecureHttp?: boolean
  /** Path of the claude executable for kind "claude-code". */
  command?: string
  temperature?: number
}

export interface AiProviderView extends AiProviderConfig {
  hasApiKey: boolean
}

/**
 * What an AI agent may access.
 *  - none:   nothing, only what the user types.
 *  - schema: table/column metadata (default).
 *  - read:   metadata + read-only queries executed inside a READ ONLY transaction.
 */
export type AiAccessLevel = 'none' | 'schema' | 'read'

export interface ChatMessage {
  role: 'user' | 'assistant'
  content: string
}

export interface ChatRequest {
  requestId: string
  providerId: string
  messages: ChatMessage[]
  connectionId?: string
  schema?: string
  editorSql?: string
  /** Claude Code session to resume. */
  sessionId?: string
}

export type ChatEvent =
  | { requestId: string; type: 'text'; text: string }
  | { requestId: string; type: 'tool'; name: string; detail: string }
  | { requestId: string; type: 'session'; sessionId: string }
  | { requestId: string; type: 'done' }
  | { requestId: string; type: 'error'; error: string }

// ---------------------------------------------------------------------------------------------
// Settings

export type Theme = 'system' | 'dark' | 'light'
export type Language = 'system' | 'en' | 'de'

export interface TestResult {
  serverVersion: string
  hostKeyFingerprint?: string | null
}

export interface ClaudeInfo {
  path: string
  version: string
}

export interface NativeTools {
  pgDump?: string | null
  pgRestore?: string | null
  mysqldump?: string | null
  mariadbDump?: string | null
}

export interface Settings {
  theme: Theme
  language: Language
  editorFontSize: number
  maxRows: number
  confirmDestructive: boolean
  autoCommit: boolean
  aiAccess: AiAccessLevel
  aiProviders: AiProviderConfig[]
  aiDefaultProviderId: string | null
  mcpEnabled: boolean
  mcpPort: number
  /** MCP clients may ask to execute data-modifying SQL; each request needs approval in the UI. */
  mcpAllowWrite: boolean
  queryTimeoutSec: number
}

export interface SettingsView extends Omit<Settings, 'aiProviders'> {
  aiProviders: AiProviderView[]
  secureStorage: { available: boolean; backend: string }
}

export interface McpInfo {
  enabled: boolean
  running: boolean
  url: string
  token: string
  bridgePath: string
  claudeDesktopConfig: string
  claudeCodeCommand: string
}

export interface HistoryEntry {
  id: string
  connectionId: string
  sql: string
  at: number
  durationMs: number
  ok: boolean
}

export interface HostKeyPrompt {
  promptId: string
  host: string
  port: number
  fingerprint: string
  algorithm: string
  /** Previously trusted fingerprint: set when the key CHANGED (possible MITM). */
  previous?: string
}

export interface CertInfo {
  host: string
  port: number
  fingerprint: string
  subject: string
  issuer: string
  validFrom: string
  validTo: string
  pem: string
}

export interface ApprovalRequest {
  approvalId: string
  source: string
  connectionName: string
  sql: string
}
