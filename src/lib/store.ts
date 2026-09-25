// Global UI state (zustand).
import { create } from 'zustand'
import type {
  ApprovalRequest,
  CellValue,
  ColumnMeta,
  ConnectionTree,
  ConnectionView,
  DbObject,
  Dialect,
  ExportSource,
  HostKeyPrompt,
  ObjectKind,
  ProgressEvent,
  SchemaSummary,
  SettingsView,
  TableInfo
} from '@shared/types'
import { api, errorMessage } from './api'
import { t } from './i18n'

export interface SqlTab {
  id: string
  kind: 'sql'
  title: string
  connectionId: string | null
  schema?: string
  sql: string
  filePath?: string
  dirty?: boolean
}

export interface TableTab {
  id: string
  kind: 'table'
  title: string
  connectionId: string
  schema: string
  name: string
  objectKind: ObjectKind
  view: 'data' | 'structure' | 'ddl'
}

export type Tab = SqlTab | TableTab

export interface ConnState {
  status: 'connecting' | 'connected' | 'error'
  summary?: SchemaSummary
  error?: string
  inTransaction?: boolean
  autoCommit?: boolean
}

export type Dialog =
  | { type: 'connection'; connection?: ConnectionView; folderId?: string | null }
  | { type: 'settings'; section?: 'general' | 'ai' | 'mcp' | 'security' }
  | { type: 'export'; source: ExportSource; defaultName: string; dialect?: Dialect }
  | { type: 'import'; connectionId: string; schema?: string; table?: string }
  | { type: 'transfer'; sourceConnectionId?: string; schema?: string; tables?: string[] }
  | { type: 'backup'; connectionId: string; schema?: string }
  | { type: 'restore'; connectionId: string }
  | { type: 'generate'; connectionId: string; dialect: Dialect; table?: TableInfo; columns?: ColumnMeta[]; rows?: CellValue[][]; initial?: string }
  | { type: 'history'; connectionId?: string }
  | { type: 'prompt'; title: string; label: string; value?: string; password?: boolean; onSubmit: (v: string) => void | Promise<void> }
  | { type: 'confirm'; title: string; message: string; details?: string; danger?: boolean; confirmLabel?: string; onConfirm: () => void | Promise<void> }
  | { type: 'value'; title: string; value: string }
  | { type: 'about' }

export interface Toast {
  id: number
  kind: 'info' | 'success' | 'error'
  message: string
}

interface State {
  settings: SettingsView | null
  tree: ConnectionTree
  conn: Record<string, ConnState>
  objects: Record<string, DbObject[] | 'loading' | { error: string }>
  tabs: Tab[]
  activeTabId: string | null
  sidebarVisible: boolean
  chatVisible: boolean
  selectedConnectionId: string | null
  dialog: Dialog | null
  toasts: Toast[]
  tasks: Record<string, ProgressEvent>
  hostKeyPrompts: HostKeyPrompt[]
  approvals: ApprovalRequest[]

  loadSettings: () => Promise<void>
  loadTree: () => Promise<void>
  connect: (id: string) => Promise<boolean>
  disconnect: (id: string) => Promise<void>
  refreshConnection: (id: string) => Promise<void>
  loadObjects: (connectionId: string, schema: string, force?: boolean) => Promise<void>
  openSqlTab: (opts?: { connectionId?: string | null; sql?: string; title?: string; schema?: string; filePath?: string }) => string
  openTableTab: (connectionId: string, schema: string, name: string, kind: ObjectKind, view?: TableTab['view']) => void
  closeTab: (id: string) => void
  updateTab: (id: string, patch: Partial<SqlTab> | Partial<TableTab>) => void
  setActiveTab: (id: string) => void
  setDialog: (d: Dialog | null) => void
  toast: (message: string, kind?: Toast['kind']) => void
  dismissToast: (id: number) => void
  setTask: (e: ProgressEvent) => void
  removeTask: (id: string) => void
  toggleSidebar: () => void
  toggleChat: () => void
  selectConnection: (id: string | null) => void
  setTxState: (id: string, inTransaction: boolean) => void
}

let toastId = 0
const TABS_KEY = 'sqlighter.tabs.v1'

export function uid(): string {
  return Math.random().toString(36).slice(2, 10) + Date.now().toString(36)
}

function loadTabs(): { tabs: Tab[]; active: string | null } {
  try {
    const raw = localStorage.getItem(TABS_KEY)
    if (raw) {
      const v = JSON.parse(raw) as { tabs: Tab[]; active: string | null }
      if (Array.isArray(v.tabs)) return v
    }
  } catch {
    /* ignore */
  }
  return { tabs: [], active: null }
}

const initial = loadTabs()

export const useStore = create<State>((set, get) => ({
  settings: null,
  tree: { folders: [], connections: [] },
  conn: {},
  objects: {},
  tabs: initial.tabs,
  activeTabId: initial.active ?? initial.tabs[0]?.id ?? null,
  sidebarVisible: true,
  chatVisible: localStorage.getItem('sqlighter.chat') !== '0',
  selectedConnectionId: null,
  dialog: null,
  toasts: [],
  tasks: {},
  hostKeyPrompts: [],
  approvals: [],

  loadSettings: async () => {
    set({ settings: await api.getSettings() })
  },

  loadTree: async () => {
    set({ tree: await api.getTree() })
  },

  connect: async (id) => {
    const c = get().tree.connections.find((x) => x.id === id)
    if (!c) return false
    const cur = get().conn[id]
    if (cur?.status === 'connected') return true
    const doConnect = async (password?: string): Promise<boolean> => {
      set((s) => ({ conn: { ...s.conn, [id]: { status: 'connecting' } } }))
      try {
        const summary = await api.connect(id, password)
        set((s) => ({ conn: { ...s.conn, [id]: { status: 'connected', summary, autoCommit: s.settings?.autoCommit ?? true } } }))
        if (summary.defaultSchema) get().loadObjects(id, summary.defaultSchema)
        return true
      } catch (e) {
        const msg = errorMessage(e)
        set((s) => ({ conn: { ...s.conn, [id]: { status: 'error', error: msg } } }))
        get().toast(t('Connection "{name}" failed: {error}', { name: c.name, error: msg }), 'error')
        return false
      }
    }
    const needsPassword = c.type !== 'sqlite' && !c.hasPassword && !c.savePassword
    if (needsPassword) {
      return new Promise<boolean>((resolve) => {
        set({
          dialog: {
            type: 'prompt',
            title: t('Connect to {name}', { name: c.name }),
            label: t('Password for {user}', { user: c.user || '' }),
            password: true,
            onSubmit: async (pw) => resolve(await doConnect(pw))
          }
        })
      })
    }
    return doConnect()
  },

  disconnect: async (id) => {
    await api.disconnect(id).catch(() => undefined)
    set((s) => {
      const conn = { ...s.conn }
      delete conn[id]
      const objects = Object.fromEntries(Object.entries(s.objects).filter(([k]) => !k.startsWith(id + '|')))
      return { conn, objects }
    })
  },

  refreshConnection: async (id) => {
    try {
      const summary = await api.schemaSummary(id)
      set((s) => ({ conn: { ...s.conn, [id]: { ...s.conn[id], status: 'connected', summary } }, objects: Object.fromEntries(Object.entries(s.objects).filter(([k]) => !k.startsWith(id + '|'))) }))
      if (summary.defaultSchema) await get().loadObjects(id, summary.defaultSchema, true)
    } catch (e) {
      get().toast(errorMessage(e), 'error')
    }
  },

  loadObjects: async (connectionId, schema, force) => {
    const key = `${connectionId}|${schema}`
    const cur = get().objects[key]
    if (cur && !force && cur !== 'loading' && !('error' in (cur as object))) return
    set((s) => ({ objects: { ...s.objects, [key]: 'loading' } }))
    try {
      const objs = await api.listObjects(connectionId, schema)
      set((s) => ({ objects: { ...s.objects, [key]: objs } }))
    } catch (e) {
      set((s) => ({ objects: { ...s.objects, [key]: { error: errorMessage(e) } } }))
    }
  },

  openSqlTab: (opts = {}) => {
    const id = uid()
    const n = get().tabs.filter((x) => x.kind === 'sql').length + 1
    const connectionId = opts.connectionId !== undefined ? opts.connectionId : (get().selectedConnectionId ?? activeConnectionId(get()))
    const tab: SqlTab = { id, kind: 'sql', title: opts.title ?? `SQL ${n}`, connectionId, sql: opts.sql ?? '', schema: opts.schema, filePath: opts.filePath }
    set((s) => ({ tabs: [...s.tabs, tab], activeTabId: id }))
    return id
  },

  openTableTab: (connectionId, schema, name, kind, view = 'data') => {
    const existing = get().tabs.find((x) => x.kind === 'table' && x.connectionId === connectionId && x.schema === schema && x.name === name)
    if (existing) {
      set((s) => ({ activeTabId: existing.id, tabs: s.tabs.map((x) => (x.id === existing.id ? { ...x, view } as Tab : x)) }))
      return
    }
    const id = uid()
    const tab: TableTab = { id, kind: 'table', title: name, connectionId, schema, name, objectKind: kind, view }
    set((s) => ({ tabs: [...s.tabs, tab], activeTabId: id }))
  },

  closeTab: (id) => {
    set((s) => {
      const idx = s.tabs.findIndex((x) => x.id === id)
      const tabs = s.tabs.filter((x) => x.id !== id)
      let active = s.activeTabId
      if (active === id) active = tabs[Math.min(idx, tabs.length - 1)]?.id ?? null
      return { tabs, activeTabId: active }
    })
  },

  updateTab: (id, patch) => set((s) => ({ tabs: s.tabs.map((x) => (x.id === id ? ({ ...x, ...patch } as Tab) : x)) })),
  setActiveTab: (id) => set({ activeTabId: id }),
  setDialog: (d) => set({ dialog: d }),

  toast: (message, kind = 'info') => {
    const id = ++toastId
    set((s) => ({ toasts: [...s.toasts, { id, kind, message }] }))
    setTimeout(() => get().dismissToast(id), kind === 'error' ? 9000 : 4000)
  },
  dismissToast: (id) => set((s) => ({ toasts: s.toasts.filter((x) => x.id !== id) })),

  setTask: (e) => set((s) => ({ tasks: { ...s.tasks, [e.taskId]: { ...s.tasks[e.taskId], ...e, label: e.label || s.tasks[e.taskId]?.label || '' } } })),
  removeTask: (id) =>
    set((s) => {
      const tasks = { ...s.tasks }
      delete tasks[id]
      return { tasks }
    }),

  toggleSidebar: () => set((s) => ({ sidebarVisible: !s.sidebarVisible })),
  toggleChat: () =>
    set((s) => {
      localStorage.setItem('sqlighter.chat', s.chatVisible ? '0' : '1')
      return { chatVisible: !s.chatVisible }
    }),
  selectConnection: (id) => set({ selectedConnectionId: id }),
  setTxState: (id, inTransaction) => set((s) => ({ conn: { ...s.conn, [id]: { ...s.conn[id], inTransaction } } }))
}))

export function activeConnectionId(s: Pick<State, 'tabs' | 'activeTabId'>): string | null {
  const tab = s.tabs.find((x) => x.id === s.activeTabId)
  return tab?.connectionId ?? null
}

// Persist SQL tabs (drafts) - debounced.
let saveTimer: ReturnType<typeof setTimeout> | undefined
useStore.subscribe((s, prev) => {
  if (s.tabs === prev.tabs && s.activeTabId === prev.activeTabId) return
  clearTimeout(saveTimer)
  saveTimer = setTimeout(() => {
    try {
      localStorage.setItem(TABS_KEY, JSON.stringify({ tabs: s.tabs, active: s.activeTabId }))
    } catch {
      /* quota */
    }
  }, 400)
})

// ---------------------------------------------------------------------------------------------
// Editor registry: lets the AI chat insert SQL into the active editor.

export interface EditorHandle {
  insert: (text: string) => void
  replace: (text: string) => void
  getSql: () => string
  getSelection: () => string
  run: (sql?: string) => void
  focus: () => void
}

export const editors = new Map<string, EditorHandle>()
