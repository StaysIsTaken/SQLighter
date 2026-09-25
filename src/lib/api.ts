// Typed wrappers around the Rust commands (see src-tauri/src/commands.rs & friends).
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type {
  ApprovalRequest,
  BackupOptions,
  CertInfo,
  ChatEvent,
  ChatRequest,
  ClaudeInfo,
  ConnectionConfig,
  ConnectionSecrets,
  ConnectionTree,
  ConnectionView,
  DbObject,
  ExecuteOptions,
  ExecuteResponse,
  ExportOptions,
  ExportSource,
  Folder,
  HistoryEntry,
  HostKeyPrompt,
  ImportOptions,
  ImportPreview,
  ImportSourceOptions,
  McpInfo,
  NativeTools,
  ObjectKind,
  ProgressEvent,
  QueryResult,
  RestoreOptions,
  SchemaSummary,
  Settings,
  SettingsView,
  TableDataRequest,
  TableInfo,
  TestResult,
  TransferOptions
} from '@shared/types'

export interface FileFilter {
  name: string
  extensions: string[]
}

export const api = {
  // settings
  getSettings: () => invoke<SettingsView>('get_settings'),
  saveSettings: (settings: Settings) => invoke<SettingsView>('save_settings', { settings }),
  setAiApiKey: (providerId: string, apiKey: string | null) => invoke<void>('set_ai_api_key', { providerId, apiKey }),

  // connections
  getTree: () => invoke<ConnectionTree>('get_tree'),
  saveConnection: (config: ConnectionConfig, secrets?: ConnectionSecrets) => invoke<ConnectionView>('save_connection', { config, secrets: secrets ?? null }),
  deleteConnection: (id: string) => invoke<void>('delete_connection', { id }),
  duplicateConnection: (id: string) => invoke<ConnectionView>('duplicate_connection', { id }),
  saveFolder: (folder: Folder) => invoke<Folder>('save_folder', { folder }),
  deleteFolder: (id: string) => invoke<void>('delete_folder', { id }),
  moveItem: (kind: 'folder' | 'connection', id: string, folderId: string | null, order?: number) =>
    invoke<void>('move_item', { kind, id, folderId, order: order ?? null }),
  testConnection: (config: ConnectionConfig, secrets?: ConnectionSecrets) => invoke<TestResult>('test_connection', { config, secrets: secrets ?? null }),
  fetchServerCert: (config: ConnectionConfig, secrets?: ConnectionSecrets) => invoke<CertInfo>('fetch_server_cert', { config, secrets: secrets ?? null }),
  answerHostKey: (promptId: string, accept: boolean) => invoke<void>('answer_host_key', { promptId, accept }),
  answerApproval: (approvalId: string, approve: boolean) => invoke<void>('answer_approval', { approvalId, approve }),

  // sessions
  connect: (id: string, password?: string) => invoke<SchemaSummary>('connect', { id, password: password ?? null }),
  disconnect: (id: string) => invoke<void>('disconnect', { id }),
  connectedIds: () => invoke<string[]>('connected_ids'),
  schemaSummary: (id: string) => invoke<SchemaSummary>('schema_summary', { id }),
  execute: (connectionId: string, sql: string, options?: ExecuteOptions) => invoke<ExecuteResponse>('execute', { connectionId, sql, options: options ?? null }),
  cancelQuery: (connectionId: string) => invoke<void>('cancel_query', { connectionId }),
  commit: (connectionId: string) => invoke<void>('commit', { connectionId }),
  rollback: (connectionId: string) => invoke<void>('rollback', { connectionId }),
  setAutoCommit: (connectionId: string, on: boolean) => invoke<void>('set_auto_commit', { connectionId, on }),

  // metadata
  listObjects: (connectionId: string, schema: string) => invoke<DbObject[]>('list_objects', { connectionId, schema }),
  describeTable: (connectionId: string, schema: string, name: string, kind?: ObjectKind) =>
    invoke<TableInfo>('describe_table', { connectionId, schema, name, kind: kind ?? null }),
  getDdl: (connectionId: string, schema: string, name: string, kind: ObjectKind) => invoke<string>('get_ddl', { connectionId, schema, name, kind }),
  completionSchema: (connectionId: string, schema: string) => invoke<Record<string, string[]>>('completion_schema', { connectionId, schema }),
  tableData: (request: TableDataRequest) => invoke<QueryResult>('table_data', { request }),
  applyChanges: (connectionId: string, statements: string[]) => invoke<number>('apply_changes', { connectionId, statements }),

  // history
  getHistory: (connectionId?: string) => invoke<HistoryEntry[]>('get_history', { connectionId: connectionId ?? null }),
  clearHistory: () => invoke<void>('clear_history'),

  // files
  pickOpenFile: (filters: FileFilter[]) => invoke<string | null>('pick_open_file', { filters }),
  pickSaveFile: (defaultName: string, filters: FileFilter[]) => invoke<string | null>('pick_save_file', { defaultName, filters }),
  readTextFile: (path: string) => invoke<string>('read_text_file', { path }),
  writeTextFile: (path: string, content: string) => invoke<void>('write_text_file', { path, content }),

  // io
  exportData: (source: ExportSource, options: ExportOptions) => invoke<string>('export_data', { source, options }),
  importPreview: (source: ImportSourceOptions) => invoke<ImportPreview>('import_preview', { source }),
  importData: (options: ImportOptions) => invoke<string>('import_data', { options }),
  transferData: (options: TransferOptions) => invoke<string>('transfer_data', { options }),
  backup: (options: BackupOptions) => invoke<string>('backup', { options }),
  restore: (options: RestoreOptions) => invoke<string>('restore', { options }),
  cancelTask: (taskId: string) => invoke<void>('cancel_task', { taskId }),
  nativeTools: () => invoke<NativeTools>('native_tools'),

  // ai
  aiChat: (request: ChatRequest) => invoke<void>('ai_chat', { request }),
  aiCancel: (requestId: string) => invoke<void>('ai_cancel', { requestId }),
  aiListModels: (providerId: string) => invoke<string[]>('ai_list_models', { providerId }),
  detectClaude: (command?: string) => invoke<ClaudeInfo>('detect_claude', { command: command ?? null }),
  mcpInfo: () => invoke<McpInfo>('mcp_info'),
  mcpRegenerateToken: () => invoke<McpInfo>('mcp_regenerate_token')
}

export const events = {
  onProgress: (cb: (e: ProgressEvent) => void): Promise<UnlistenFn> => listen<ProgressEvent>('progress', (e) => cb(e.payload)),
  onAi: (cb: (e: ChatEvent) => void): Promise<UnlistenFn> => listen<ChatEvent>('ai-event', (e) => cb(e.payload)),
  onHostKey: (cb: (e: HostKeyPrompt) => void): Promise<UnlistenFn> => listen<HostKeyPrompt>('ssh-hostkey', (e) => cb(e.payload)),
  onApproval: (cb: (e: ApprovalRequest) => void): Promise<UnlistenFn> => listen<ApprovalRequest>('approval-request', (e) => cb(e.payload)),
  onOpenSql: (cb: (e: { sql: string; connectionId?: string | null; title?: string | null }) => void): Promise<UnlistenFn> =>
    listen<{ sql: string; connectionId?: string | null; title?: string | null }>('open-sql', (e) => cb(e.payload))
}

/** Error messages from commands arrive as plain strings. */
export function errorMessage(e: unknown): string {
  if (typeof e === 'string') return e
  if (e instanceof Error) return e.message
  try {
    return JSON.stringify(e)
  } catch {
    return String(e)
  }
}
