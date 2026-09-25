// AI assistant panel.
import { useEffect, useMemo, useRef, useState } from 'react'
import { Bot, Check, ClipboardCopy, CornerDownLeft, FilePlus2, Play, Plus, Replace, Send, Settings2, ShieldCheck, Square, Wrench, X } from 'lucide-react'
import type { ChatEvent, ChatMessage } from '@shared/types'
import { api, errorMessage, events } from '@/lib/api'
import { copyText } from '@/lib/format'
import { t } from '@/lib/i18n'
import { activeConnectionId, editors, uid, useStore } from '@/lib/store'
import { Markdown } from './Markdown'

interface UiMessage extends ChatMessage {
  tools?: { name: string; detail: string }[]
  error?: string
}

// Lets other components prefill the chat ("Ask AI").
const askListeners = new Set<(text: string) => void>()
export function askAi(text: string) {
  const s = useStore.getState()
  if (!s.chatVisible) s.toggleChat()
  setTimeout(() => askListeners.forEach((l) => l(text)), 0)
}

const CHAT_KEY = 'sqlighter.chat.v1'

function loadChat(): { messages: UiMessage[]; sessionId?: string } {
  try {
    return JSON.parse(localStorage.getItem(CHAT_KEY) ?? '') as { messages: UiMessage[]; sessionId?: string }
  } catch {
    return { messages: [] }
  }
}

export function ChatPanel() {
  const settings = useStore((s) => s.settings)
  const tree = useStore((s) => s.tree)
  const connState = useStore((s) => s.conn)
  const tabs = useStore((s) => s.tabs)
  const activeTabId = useStore((s) => s.activeTabId)
  const selectedConn = useStore((s) => s.selectedConnectionId)
  const { setDialog, openSqlTab } = useStore.getState()
  const initial = useMemo(loadChat, [])
  const [messages, setMessages] = useState<UiMessage[]>(initial.messages ?? [])
  const [sessionId, setSessionId] = useState<string | undefined>(initial.sessionId)
  const [input, setInput] = useState('')
  const [requestId, setRequestId] = useState<string | null>(null)
  const [providerId, setProviderId] = useState<string>(() => localStorage.getItem('sqlighter.chat.provider') ?? '')
  const [includeEditor, setIncludeEditor] = useState(true)
  const scroller = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLTextAreaElement>(null)
  const reqRef = useRef<string | null>(null)
  reqRef.current = requestId

  const providers = settings?.aiProviders ?? []
  const provider = providers.find((p) => p.id === providerId) ?? providers.find((p) => p.id === settings?.aiDefaultProviderId) ?? providers[0]
  const connectionId = activeConnectionId({ tabs, activeTabId }) ?? selectedConn
  const conn = tree.connections.find((c) => c.id === connectionId)
  const activeTab = tabs.find((x) => x.id === activeTabId)
  const schema = activeTab?.kind === 'sql' ? activeTab.schema : activeTab?.kind === 'table' ? activeTab.schema : undefined

  useEffect(() => {
    try {
      localStorage.setItem(CHAT_KEY, JSON.stringify({ messages: messages.slice(-60), sessionId }))
    } catch {
      /* ignore */
    }
  }, [messages, sessionId])

  useEffect(() => {
    const l = (text: string) => {
      setInput(text)
      setTimeout(() => inputRef.current?.focus(), 30)
    }
    askListeners.add(l)
    return () => {
      askListeners.delete(l)
    }
  }, [])

  useEffect(() => {
    const un = events.onAi((e: ChatEvent) => {
      if (e.requestId !== reqRef.current) return
      setMessages((ms) => {
        const copy = [...ms]
        const last = { ...copy[copy.length - 1] }
        if (last.role !== 'assistant') return ms
        if (e.type === 'text') last.content += e.text
        else if (e.type === 'tool') last.tools = [...(last.tools ?? []), { name: e.name, detail: e.detail }]
        else if (e.type === 'error') last.error = e.error
        copy[copy.length - 1] = last
        return copy
      })
      if (e.type === 'session') setSessionId(e.sessionId)
      if (e.type === 'done' || e.type === 'error') setRequestId(null)
    })
    return () => {
      un.then((f) => f())
    }
  }, [])

  useEffect(() => {
    const el = scroller.current
    if (el) el.scrollTop = el.scrollHeight
  }, [messages])

  // A different provider or connection starts a new Claude Code session.
  useEffect(() => setSessionId(undefined), [provider?.id, connectionId])

  const send = async (text?: string) => {
    const content = (text ?? input).trim()
    if (!content || requestId || !provider) return
    const rid = uid()
    const history: UiMessage[] = [...messages.filter((m) => !m.error || m.content), { role: 'user', content }]
    setMessages([...history, { role: 'assistant', content: '' }])
    setInput('')
    setRequestId(rid)
    const editorSql = includeEditor && activeTab?.kind === 'sql' ? editors.get(activeTab.id)?.getSql() : undefined
    try {
      if (connectionId && connState[connectionId]?.status !== 'connected' && settings?.aiAccess !== 'none') {
        await useStore.getState().connect(connectionId)
      }
      await api.aiChat({
        requestId: rid,
        providerId: provider.id,
        messages: history.map((m) => ({ role: m.role, content: m.content })),
        connectionId: connectionId ?? undefined,
        schema,
        editorSql,
        sessionId: provider.kind === 'claude-code' ? sessionId : undefined
      })
    } catch (e) {
      setMessages((ms) => {
        const copy = [...ms]
        copy[copy.length - 1] = { role: 'assistant', content: '', error: errorMessage(e) }
        return copy
      })
      setRequestId(null)
    }
  }

  const insertSql = (sql: string, mode: 'insert' | 'replace' | 'new' | 'run') => {
    const tab = useStore.getState().tabs.find((x) => x.id === useStore.getState().activeTabId)
    const ed = tab?.kind === 'sql' ? editors.get(tab.id) : undefined
    if (mode === 'new' || !ed) {
      const id = openSqlTab({ connectionId, sql, schema })
      if (mode === 'run') setTimeout(() => editors.get(id)?.run(sql), 150)
      return
    }
    if (mode === 'insert') ed.insert(sql)
    else if (mode === 'replace') ed.replace(sql)
    else ed.run(sql)
  }

  const accessLabel = settings?.aiAccess === 'none' ? t('no database access') : settings?.aiAccess === 'read' ? t('schema + read-only queries') : t('schema only')

  const suggestions = conn
    ? [t('Which tables are there and how are they related?'), t('Write a query for the 10 most recent rows of the largest table'), t('Find possible missing indexes')]
    : [t('How do I write a window function?'), t('Explain the difference between INNER and LEFT JOIN')]

  return (
    <>
      <div className="panel-header">
        <Bot size={15} color="var(--accent)" />
        <span className="panel-title grow">{t('AI assistant')}</span>
        <button
          className="icon-btn"
          title={t('New chat')}
          onClick={() => {
            if (requestId) api.aiCancel(requestId)
            setMessages([])
            setSessionId(undefined)
            setRequestId(null)
          }}
        >
          <Plus size={16} />
        </button>
        <button className="icon-btn" title={t('AI settings')} onClick={() => setDialog({ type: 'settings', section: 'ai' })}>
          <Settings2 size={15} />
        </button>
        <button className="icon-btn" title={t('Close (Ctrl+J)')} onClick={() => useStore.getState().toggleChat()}>
          <X size={15} />
        </button>
      </div>
      <div className="toolbar" style={{ flexWrap: 'wrap', gap: 6 }}>
        <select
          className="select sm"
          style={{ flex: 1, minWidth: 120 }}
          value={provider?.id ?? ''}
          onChange={(e) => {
            setProviderId(e.target.value)
            localStorage.setItem('sqlighter.chat.provider', e.target.value)
          }}
        >
          {providers.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
              {p.model ? ` · ${p.model}` : ''}
            </option>
          ))}
        </select>
        <span className="badge" title={t('Database access for the AI (Settings → AI)')}>
          <ShieldCheck size={11} /> {accessLabel}
        </span>
      </div>
      <div className="chat-messages" ref={scroller}>
        {messages.length === 0 && (
          <div className="chat-empty">
            <Bot size={30} style={{ opacity: 0.6 }} />
            <div>
              {conn ? t('Ask anything about "{name}". The assistant writes SQL; you decide what runs.', { name: conn.name }) : t('Ask for SQL, explanations or optimizations.')}
            </div>
            <div className="col" style={{ alignItems: 'center' }}>
              {suggestions.map((s) => (
                <button key={s} className="suggestion" onClick={() => send(s)}>
                  {s}
                </button>
              ))}
            </div>
          </div>
        )}
        {messages.map((m, i) =>
          m.role === 'user' ? (
            <div key={i} className="msg user">
              {m.content}
            </div>
          ) : (
            <div key={i} className={`msg assistant ${requestId && i === messages.length - 1 && !m.content ? 'cursor-blink' : ''}`}>
              {m.tools?.map((tl, j) => (
                <div key={j} className="tool-chip" title={tl.detail}>
                  <Wrench size={11} /> {tl.name}
                  {tl.detail && <span className="ellipsis mono muted">{tl.detail}</span>}
                </div>
              ))}
              <Markdown
                text={m.content}
                code={(code, lang, k) => <CodeBlock key={k} code={code} lang={lang} onAction={insertSql} />}
              />
              {requestId && i === messages.length - 1 && m.content && <span className="cursor-blink" />}
              {m.error && <div className="error-box">{m.error}</div>}
            </div>
          )
        )}
      </div>
      <div className="chat-input">
        <textarea
          ref={inputRef}
          value={input}
          placeholder={provider ? t('Message {name}… (Enter to send, Shift+Enter for a new line)', { name: provider.name }) : t('Configure an AI provider in the settings')}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault()
              send()
            }
          }}
        />
        <div className="row">
          <label className="check small muted" title={t('Send the SQL of the active editor as context')}>
            <input type="checkbox" checked={includeEditor} onChange={(e) => setIncludeEditor(e.target.checked)} />
            {t('Editor SQL')}
          </label>
          <span className="small muted ellipsis grow" title={conn ? `${conn.name}${schema ? ' · ' + schema : ''}` : ''}>
            {conn ? `${t('Context')}: ${conn.name}${schema ? ' · ' + schema : ''}` : t('No connection selected')}
          </span>
          {requestId ? (
            <button
              className="btn small"
              onClick={() => {
                api.aiCancel(requestId)
                setRequestId(null)
              }}
            >
              <Square size={12} /> {t('Stop')}
            </button>
          ) : (
            <button className="btn small primary" disabled={!input.trim() || !provider} onClick={() => send()}>
              <Send size={12} /> {t('Send')}
            </button>
          )}
        </div>
      </div>
    </>
  )
}

function CodeBlock({ code, lang, onAction }: { code: string; lang: string; onAction: (sql: string, mode: 'insert' | 'replace' | 'new' | 'run') => void }) {
  const [copied, setCopied] = useState(false)
  const isSql = !lang || ['sql', 'pgsql', 'plsql', 'mysql', 'tsql', 'sqlite', 'postgresql'].includes(lang)
  return (
    <div className="codeblock">
      <div className="codeblock-head">
        <span className="grow">{lang || 'sql'}</span>
        <button
          className="icon-btn sm"
          title={t('Copy')}
          onClick={() =>
            copyText(code).then(() => {
              setCopied(true)
              setTimeout(() => setCopied(false), 1200)
            })
          }
        >
          {copied ? <Check size={13} /> : <ClipboardCopy size={13} />}
        </button>
        {isSql && (
          <>
            <button className="icon-btn sm" title={t('Insert at cursor')} onClick={() => onAction(code, 'insert')}>
              <CornerDownLeft size={13} />
            </button>
            <button className="icon-btn sm" title={t('Replace editor content')} onClick={() => onAction(code, 'replace')}>
              <Replace size={13} />
            </button>
            <button className="icon-btn sm" title={t('Open in new editor tab')} onClick={() => onAction(code, 'new')}>
              <FilePlus2 size={13} />
            </button>
            <button className="icon-btn sm" title={t('Run (asks before destructive statements)')} onClick={() => onAction(code, 'run')}>
              <Play size={13} />
            </button>
          </>
        )}
      </div>
      <pre className="selectable">{code}</pre>
    </div>
  )
}

