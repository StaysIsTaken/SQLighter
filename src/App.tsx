// Application shell: top bar, sidebar, tabs, AI panel, status bar, dialogs.
import { useEffect } from 'react'
import {
  ArrowRightLeft,
  Bot,
  CheckCircle2,
  CircleAlert,
  Database,
  DatabaseBackup,
  FileCode2,
  FolderOpen,
  Info,
  Loader2,
  PanelLeft,
  PlugZap,
  Settings,
  Table2,
  Upload,
  X
} from 'lucide-react'
import { api, errorMessage, events } from '@/lib/api'
import { t } from '@/lib/i18n'
import { activeConnectionId, useStore } from '@/lib/store'
import { ChatPanel } from '@/components/ChatPanel'
import { Sidebar } from '@/components/Sidebar'
import { SqlTab } from '@/components/SqlTab'
import { TableTab } from '@/components/TableTab'
import { DbIcon, Logo, Splitter, usePersistentState } from '@/components/ui'
import { ConnectionDialog } from '@/components/dialogs/ConnectionDialog'
import { GenerateDialog } from '@/components/dialogs/GenerateDialog'
import { BackupDialog, ExportDialog, ImportDialog, RestoreDialog, TransferDialog } from '@/components/dialogs/IoDialogs'
import { SettingsDialog } from '@/components/dialogs/SettingsDialog'
import { AboutDialog, ApprovalDialog, ConfirmDialog, HistoryDialog, HostKeyDialog, PasswordPrompt, PromptDialog, ValueDialog } from '@/components/dialogs/SmallDialogs'

export function App() {
  const s = useStore()
  const [sideW, setSideW] = usePersistentState('sqlighter.sideW', 280)
  const [chatW, setChatW] = usePersistentState('sqlighter.chatW', 380)
  const activeTab = s.tabs.find((x) => x.id === s.activeTabId)
  const connId = activeConnectionId(s) ?? s.selectedConnectionId
  const conn = s.tree.connections.find((c) => c.id === connId)
  const connState = connId ? s.conn[connId] : undefined

  // Backend events
  useEffect(() => {
    const subs = [
      events.onProgress((e) => {
        useStore.getState().setTask(e)
        if (e.finished) {
          if (e.error) useStore.getState().toast(`${e.label || t('Task')}: ${e.error}`, 'error')
          else useStore.getState().toast(e.message ?? t('Done'), 'success')
          setTimeout(() => useStore.getState().removeTask(e.taskId), 1500)
        }
      }),
      events.onHostKey((p) => useStore.setState((st) => ({ hostKeyPrompts: [...st.hostKeyPrompts, p] }))),
      events.onApproval((a) => useStore.setState((st) => ({ approvals: [...st.approvals, a] }))),
      events.onOpenSql((e) => {
        useStore.getState().openSqlTab({ sql: e.sql, connectionId: e.connectionId ?? undefined, title: e.title ?? undefined })
        useStore.getState().toast(t('An AI agent opened SQL in a new tab'), 'info')
      })
    ]
    return () => subs.forEach((p) => p.then((un) => un()))
  }, [])

  // Global shortcuts
  useEffect(() => {
    const onKey = async (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey
      if (!mod) return
      const k = e.key.toLowerCase()
      const st = useStore.getState()
      if (k === 't' && !e.shiftKey) {
        e.preventDefault()
        st.openSqlTab()
      } else if (k === 'w') {
        e.preventDefault()
        if (st.activeTabId) st.closeTab(st.activeTabId)
      } else if (k === 'j') {
        e.preventDefault()
        st.toggleChat()
      } else if (k === 'b') {
        e.preventDefault()
        st.toggleSidebar()
      } else if (k === 'o') {
        e.preventDefault()
        openFile()
      } else if (k === ',') {
        e.preventDefault()
        st.setDialog({ type: 'settings' })
      } else if (e.key === 'Tab') {
        e.preventDefault()
        const i = st.tabs.findIndex((x) => x.id === st.activeTabId)
        const n = st.tabs[(i + (e.shiftKey ? -1 : 1) + st.tabs.length) % st.tabs.length]
        if (n) st.setActiveTab(n.id)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  const tasks = Object.values(s.tasks)

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <Logo />
          SQLighter
        </div>
        <button className={`icon-btn ${s.sidebarVisible ? 'active' : ''}`} onClick={s.toggleSidebar} title={t('Toggle sidebar (Ctrl+B)')}>
          <PanelLeft size={16} />
        </button>
        <button className="btn small ghost" onClick={() => s.setDialog({ type: 'connection' })}>
          <PlugZap size={14} /> {t('New connection')}
        </button>
        <button className="btn small ghost" onClick={() => s.openSqlTab()}>
          <FileCode2 size={14} /> {t('SQL editor')}
        </button>
        <button className="btn small ghost" onClick={openFile} title={t('Open SQL file (Ctrl+O)')}>
          <FolderOpen size={14} /> {t('Open')}
        </button>
        <div className="toolbar-sep" />
        <button className="btn small ghost" disabled={!connId} onClick={() => connId && s.setDialog({ type: 'import', connectionId: connId })}>
          <Upload size={14} /> {t('Import')}
        </button>
        <button className="btn small ghost" onClick={() => s.setDialog({ type: 'transfer', sourceConnectionId: connId ?? undefined })}>
          <ArrowRightLeft size={14} /> {t('Transfer')}
        </button>
        <button className="btn small ghost" disabled={!connId} onClick={() => connId && s.setDialog({ type: 'backup', connectionId: connId })}>
          <DatabaseBackup size={14} /> {t('Backup')}
        </button>
        <div className="grow" />
        <button className={`btn small ${s.chatVisible ? 'primary' : ''}`} onClick={s.toggleChat} title={t('AI assistant (Ctrl+J)')}>
          <Bot size={14} /> {t('AI')}
        </button>
        <button className="icon-btn" onClick={() => s.setDialog({ type: 'settings' })} title={t('Settings (Ctrl+,)')}>
          <Settings size={16} />
        </button>
        <button className="icon-btn" onClick={() => s.setDialog({ type: 'about' })} title={t('About')}>
          <Info size={16} />
        </button>
      </header>

      <div className="main">
        {s.sidebarVisible && (
          <>
            <aside className="sidebar" style={{ width: sideW }}>
              <Sidebar />
            </aside>
            <Splitter width={sideW} onChange={setSideW} sign={1} min={200} max={600} />
          </>
        )}
        <main className="workspace">
          <div className="tabbar">
            {s.tabs.map((tab) => {
              const tc = s.tree.connections.find((c) => c.id === tab.connectionId)
              return (
                <div
                  key={tab.id}
                  className={`tab ${tab.id === s.activeTabId ? 'active' : ''}`}
                  onClick={() => s.setActiveTab(tab.id)}
                  onMouseDown={(e) => e.button === 1 && (e.preventDefault(), s.closeTab(tab.id))}
                  title={tc ? `${tab.title} · ${tc.name}` : tab.title}
                >
                  {tab.kind === 'sql' ? <FileCode2 size={14} color={tc?.color} /> : <Table2 size={14} color={tc?.color} />}
                  <span className="ellipsis grow">{tab.title}</span>
                  {tab.kind === 'sql' && tab.dirty && <span className="dirty" />}
                  <button
                    className="icon-btn sm tab-close"
                    onClick={(e) => {
                      e.stopPropagation()
                      s.closeTab(tab.id)
                    }}
                  >
                    <X size={13} />
                  </button>
                </div>
              )
            })}
            <button className="icon-btn" style={{ margin: 'auto 4px' }} onClick={() => s.openSqlTab()} title={t('New SQL editor (Ctrl+T)')}>
              +
            </button>
          </div>
          {s.tabs.map((tab) =>
            tab.kind === 'sql' ? <SqlTab key={tab.id} tab={tab} visible={tab.id === s.activeTabId} /> : <TableTab key={tab.id} tab={tab} visible={tab.id === s.activeTabId} />
          )}
          {!activeTab && <Welcome />}
        </main>
        {s.chatVisible && (
          <>
            <Splitter width={chatW} onChange={setChatW} sign={-1} min={300} max={800} />
            <aside className="chat" style={{ width: chatW }}>
              <ChatPanel />
            </aside>
          </>
        )}
      </div>

      <footer className="statusbar">
        {conn ? (
          <>
            <span className="row" style={{ gap: 6 }}>
              <DbIcon type={conn.type} size={14} />
              {conn.name}
            </span>
            {connState?.status === 'connected' && (
              <>
                <span className="row" style={{ gap: 5 }}>
                  <span className="dot" style={{ background: 'var(--ok)' }} />
                  {connState.summary?.serverVersion.split(/[\n,(]/)[0].slice(0, 60)}
                </span>
                <span>{connState.autoCommit === false ? t('Manual commit') : t('Auto-commit')}</span>
                {connState.inTransaction && <span className="badge warn">{t('Transaction open')}</span>}
              </>
            )}
            {connState?.status === 'connecting' && (
              <span className="row" style={{ gap: 5 }}>
                <Loader2 size={12} className="spin" /> {t('Connecting…')}
              </span>
            )}
            {!connState && <span className="muted">{t('Not connected')}</span>}
            {conn.readOnly && <span className="badge">{t('read-only')}</span>}
            {conn.production && <span className="badge danger">{t('production')}</span>}
          </>
        ) : (
          <span className="muted row" style={{ gap: 6 }}>
            <Database size={12} /> {t('No connection selected')}
          </span>
        )}
        <span className="sep" />
        {s.settings && !s.settings.secureStorage.available && (
          <span className="badge warn" title={t('No system keychain available - secrets are kept in memory only')}>
            {t('Secrets: session only')}
          </span>
        )}
        <span className="muted">{t('Ctrl+Enter run · Ctrl+J AI')}</span>
      </footer>

      {(tasks.length > 0 || s.toasts.length > 0) && (
        <div className="toasts">
          {tasks.map((task) => (
            <div key={task.taskId} className="toast" style={{ flexDirection: 'column', alignItems: 'stretch', gap: 6 }}>
              <div className="row">
                {task.finished ? task.error ? <CircleAlert size={15} color="var(--danger)" /> : <CheckCircle2 size={15} color="var(--ok)" /> : <Loader2 size={15} className="spin" />}
                <b className="grow ellipsis">{task.label}</b>
                {!task.finished && (
                  <button className="btn small ghost" onClick={() => api.cancelTask(task.taskId)}>
                    {t('Cancel')}
                  </button>
                )}
              </div>
              {!task.finished && (
                <div className={`progress ${task.total ? '' : 'indeterminate'}`}>
                  <div style={{ width: task.total ? `${Math.min(100, (task.done / task.total) * 100)}%` : '35%' }} />
                </div>
              )}
              <span className="small muted ellipsis">{task.message ?? (task.done ? `${task.done.toLocaleString()}${task.total ? ` / ${task.total.toLocaleString()}` : ''}` : '')}</span>
            </div>
          ))}
          {s.toasts.map((x) => (
            <ToastView key={x.id} toast={x} />
          ))}
        </div>
      )}

      <DialogHost />
      {s.passwordPrompts[0] && (
        <PasswordPrompt
          key={s.passwordPrompts[0].id}
          title={s.passwordPrompts[0].title}
          label={s.passwordPrompts[0].label}
          onDone={(pw) => {
            const p = useStore.getState().passwordPrompts[0]
            useStore.setState((st) => ({ passwordPrompts: st.passwordPrompts.slice(1) }))
            p?.resolve(pw)
          }}
        />
      )}
      {s.hostKeyPrompts[0] && <HostKeyDialog prompt={s.hostKeyPrompts[0]} onDone={() => useStore.setState((st) => ({ hostKeyPrompts: st.hostKeyPrompts.slice(1) }))} />}
      {!s.hostKeyPrompts[0] && s.approvals[0] && <ApprovalDialog req={s.approvals[0]} onDone={() => useStore.setState((st) => ({ approvals: st.approvals.slice(1) }))} />}
    </div>
  )
}

function ToastView({ toast }: { toast: { id: number; kind: 'info' | 'success' | 'error'; message: string } }) {
  return (
    <div className={`toast ${toast.kind}`} onClick={() => useStore.getState().dismissToast(toast.id)}>
      {toast.kind === 'error' ? <CircleAlert size={16} /> : toast.kind === 'success' ? <CheckCircle2 size={16} /> : <Info size={16} color="var(--accent)" />}
      <span className="grow">{toast.message}</span>
    </div>
  )
}

async function openFile() {
  const st = useStore.getState()
  const path = await api.pickOpenFile([
    { name: 'SQL', extensions: ['sql', 'txt'] },
    { name: t('All files'), extensions: ['*'] }
  ])
  if (!path) return
  try {
    const sql = await api.readTextFile(path)
    st.openSqlTab({ sql, filePath: path, title: path.split(/[\\/]/).pop() })
  } catch (e) {
    st.toast(errorMessage(e), 'error')
  }
}

function Welcome() {
  const st = useStore.getState()
  const hasConnections = useStore((s) => s.tree.connections.length > 0)
  return (
    <div className="welcome">
      <div className="welcome-card">
        <Logo size={56} />
        <h1>SQLighter</h1>
        <p>{t('A calm place for your databases. Connect, query, edit data and let the AI assistant write SQL for you.')}</p>
        <div className="welcome-grid">
          <button onClick={() => st.setDialog({ type: 'connection' })}>
            <PlugZap size={18} color="var(--accent)" />
            <span>
              <b>{t('New connection')}</b>
              <div className="hint">PostgreSQL, MySQL, SQLite, …</div>
            </span>
          </button>
          <button onClick={() => st.openSqlTab()}>
            <FileCode2 size={18} color="var(--accent)" />
            <span>
              <b>{t('SQL editor')}</b>
              <div className="hint">Ctrl+T</div>
            </span>
          </button>
          <button onClick={openFile}>
            <FolderOpen size={18} color="var(--accent)" />
            <span>
              <b>{t('Open SQL file')}</b>
              <div className="hint">Ctrl+O</div>
            </span>
          </button>
          <button onClick={() => st.setDialog({ type: 'settings', section: 'ai' })}>
            <Bot size={18} color="var(--accent)" />
            <span>
              <b>{t('Set up AI')}</b>
              <div className="hint">Ollama, OpenAI, Claude</div>
            </span>
          </button>
        </div>
        {!hasConnections && <p className="hint">{t('Tip: organise connections in folders by dragging them in the sidebar.')}</p>}
      </div>
    </div>
  )
}

function DialogHost() {
  const d = useStore((s) => s.dialog)
  if (!d) return null
  switch (d.type) {
    case 'connection':
      return <ConnectionDialog connection={d.connection} folderId={d.folderId} />
    case 'settings':
      return <SettingsDialog section={d.section} />
    case 'export':
      return <ExportDialog source={d.source} defaultName={d.defaultName} dialect={d.dialect} />
    case 'import':
      return <ImportDialog connectionId={d.connectionId} schema={d.schema} table={d.table} />
    case 'transfer':
      return <TransferDialog sourceConnectionId={d.sourceConnectionId} schema={d.schema} tables={d.tables} />
    case 'backup':
      return <BackupDialog connectionId={d.connectionId} schema={d.schema} />
    case 'restore':
      return <RestoreDialog connectionId={d.connectionId} />
    case 'generate':
      return <GenerateDialog connectionId={d.connectionId} dialect={d.dialect} table={d.table} columns={d.columns} rows={d.rows} />
    case 'history':
      return <HistoryDialog connectionId={d.connectionId} />
    case 'prompt':
      return <PromptDialog {...d} />
    case 'confirm':
      return <ConfirmDialog {...d} />
    case 'value':
      return <ValueDialog title={d.title} value={d.value} />
    case 'about':
      return <AboutDialog />
  }
}
