// AI assistant panel.
import { useEffect, useRef, useState } from 'react'
import { AlertTriangle, ArrowDown, Bot, Check, ClipboardCopy, CornerDownLeft, FilePlus2, MessagesSquare, Play, Plus, Replace, Send, Settings2, ShieldCheck, Square, Trash2, Wrench, X } from 'lucide-react'
import type { ChatEvent, ChatMessage, ChatSummary } from '@shared/types'
import { api, errorMessage, events } from '@/lib/api'
import { copyText } from '@/lib/format'
import { t } from '@/lib/i18n'
import { activeConnectionId, editors, uid, useStore } from '@/lib/store'
import { Markdown } from './Markdown'

interface UiMessage extends ChatMessage {
  tools?: { name: string; detail: string }[]
  error?: string
  notices?: string[]
}

function noticeText(code: string): string {
  if (code === 'context-overflow')
    return t('The conversation is too long for this local model: the beginning (instructions, your question) may be cut off. Start a new chat, ask more specifically, or use a model with a larger context window.')
  return code
}

// Lets other components prefill the chat ("Ask AI").
const askListeners = new Set<(text: string) => void>()
export function askAi(text: string) {
  const s = useStore.getState()
  if (!s.chatVisible) s.toggleChat()
  setTimeout(() => askListeners.forEach((l) => l(text)), 0)
}

/** Chat of earlier versions, kept in localStorage; moved into the chat list once. */
const LEGACY_CHAT_KEY = 'sqlighter.chat.v1'
const RETENTION_DAYS = 7

function chatTitle(messages: UiMessage[]): string {
  const first = messages.find((m) => m.role === 'user')?.content ?? ''
  const line = first.replace(/\s+/g, ' ').trim()
  return line.length > 60 ? line.slice(0, 57) + '…' : line || t('New chat')
}

function formatWhen(ts: number): string {
  const d = new Date(ts)
  const today = new Date()
  return d.toDateString() === today.toDateString()
    ? d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
    : d.toLocaleDateString(undefined, { weekday: 'short', day: '2-digit', month: '2-digit' })
}

export function ChatPanel() {
  const settings = useStore((s) => s.settings)
  const tree = useStore((s) => s.tree)
  const connState = useStore((s) => s.conn)
  const tabs = useStore((s) => s.tabs)
  const activeTabId = useStore((s) => s.activeTabId)
  const selectedConn = useStore((s) => s.selectedConnectionId)
  const { setDialog, openSqlTab } = useStore.getState()
  const [messages, setMessages] = useState<UiMessage[]>([])
  const [sessionId, setSessionId] = useState<string | undefined>()
  const [chatId, setChatId] = useState(uid)
  const createdAt = useRef(Date.now())
  const [chats, setChats] = useState<ChatSummary[]>([])
  const [showList, setShowList] = useState(false)
  const [atBottom, setAtBottom] = useState(true)
  // Follow new text only while the user is at the bottom; scrolling up stops following.
  const stick = useRef(true)
  // Only new messages count as activity: opening a chat must not extend its retention.
  const dirty = useRef(false)
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

  const refreshChats = () =>
    api
      .chatList()
      .then(setChats)
      .catch(() => {})

  const persist = (id: string, msgs: UiMessage[], session: string | undefined) => {
    if (!msgs.some((m) => m.role === 'user')) return
    api
      .chatSave({ id, title: chatTitle(msgs), createdAt: createdAt.current, updatedAt: 0, sessionId: session, messages: msgs })
      .then(refreshChats)
      .catch(() => {})
  }

  // Initial load: migrate the old single chat, then open the most recent one.
  useEffect(() => {
    ;(async () => {
      try {
        const legacy = localStorage.getItem(LEGACY_CHAT_KEY)
        if (legacy) {
          const old = JSON.parse(legacy) as { messages?: UiMessage[]; sessionId?: string }
          if (old.messages?.some((m) => m.role === 'user')) {
            await api.chatSave({ id: uid(), title: chatTitle(old.messages), createdAt: 0, updatedAt: 0, sessionId: old.sessionId, messages: old.messages })
          }
          localStorage.removeItem(LEGACY_CHAT_KEY)
        }
      } catch {
        /* ignore */
      }
      const list = await api.chatList().catch(() => [] as ChatSummary[])
      setChats(list)
      if (list[0]) await openChat(list[0].id)
    })()
  }, [])

  // Save when an answer is complete (and after sending, see send()).
  useEffect(() => {
    if (requestId || !dirty.current) return
    const h = setTimeout(() => {
      dirty.current = false
      persist(chatId, messages, sessionId)
    }, 300)
    return () => clearTimeout(h)
  }, [messages, sessionId, requestId, chatId])

  const scrollToBottom = () => {
    const el = scroller.current
    if (el) el.scrollTop = el.scrollHeight
    stick.current = true
    setAtBottom(true)
  }

  const openChat = async (id: string) => {
    if (reqRef.current) api.aiCancel(reqRef.current)
    setRequestId(null)
    const c = await api.chatGet(id).catch(() => null)
    if (!c) {
      refreshChats()
      return
    }
    dirty.current = false
    setChatId(c.id)
    createdAt.current = c.createdAt
    setMessages(c.messages as UiMessage[])
    setSessionId(c.sessionId)
    setShowList(false)
    stick.current = true
    setTimeout(scrollToBottom, 0)
  }

  const newChat = (keepList = false) => {
    if (reqRef.current) api.aiCancel(reqRef.current)
    dirty.current = false
    setRequestId(null)
    setChatId(uid())
    createdAt.current = Date.now()
    setMessages([])
    setSessionId(undefined)
    if (!keepList) {
      setShowList(false)
      setTimeout(() => inputRef.current?.focus(), 30)
    }
    stick.current = true
  }

  const deleteChat = async (id: string) => {
    await api.chatDelete(id).catch(() => {})
    if (id === chatId) newChat(true)
    refreshChats()
  }

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
      dirty.current = true
      setMessages((ms) => {
        const copy = [...ms]
        const last = { ...copy[copy.length - 1] }
        if (last.role !== 'assistant') return ms
        if (e.type === 'text') last.content += e.text
        else if (e.type === 'tool') last.tools = [...(last.tools ?? []), { name: e.name, detail: e.detail }]
        else if (e.type === 'error') last.error = e.error
        else if (e.type === 'notice') last.notices = [...(last.notices ?? []), e.code]
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
    if (el && stick.current) el.scrollTop = el.scrollHeight
  }, [messages])

  const onScroll = () => {
    const el = scroller.current
    if (!el) return
    const bottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40
    stick.current = bottom
    setAtBottom(bottom)
  }
  // Scrolling up must win against new text arriving in the same frame.
  const leaveBottom = () => {
    stick.current = false
  }

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
    persist(chatId, history, sessionId)
    dirty.current = true
    setShowList(false)
    stick.current = true
    setTimeout(scrollToBottom, 0)
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
    else {
      // Show what runs: an empty editor receives the statement first.
      if (!ed.getSql().trim()) ed.replace(sql)
      ed.run(sql)
    }
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
          className={`icon-btn ${showList ? 'active' : ''}`}
          title={t('Chats')}
          onClick={() => {
            if (!showList) refreshChats()
            setShowList(!showList)
          }}
        >
          <MessagesSquare size={15} />
        </button>
        <button className="icon-btn" title={t('New chat')} onClick={() => newChat()}>
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
      {showList && (
        <div className="chat-list">
          <button className="btn small primary" onClick={() => newChat()} style={{ alignSelf: 'flex-start' }}>
            <Plus size={13} /> {t('New chat')}
          </button>
          {chats.map((c) => (
            <div
              key={c.id}
              className={`chat-item ${c.id === chatId ? 'on' : ''}`}
              onClick={() => openChat(c.id)}
              title={t('Deleted automatically on {date}', { date: new Date(c.updatedAt + RETENTION_DAYS * 86400000).toLocaleDateString() })}
            >
              <MessagesSquare size={13} className="muted" />
              <span className="grow ellipsis">{c.title}</span>
              <span className="small muted">{formatWhen(c.updatedAt)}</span>
              <button
                className="icon-btn sm"
                title={t('Delete chat')}
                onClick={(e) => {
                  e.stopPropagation()
                  deleteChat(c.id)
                }}
              >
                <Trash2 size={13} />
              </button>
            </div>
          ))}
          {!chats.length && <div className="hint">{t('No saved chats yet.')}</div>}
          <div className="hint">{t('Chats are deleted automatically 7 days after the last message.')}</div>
        </div>
      )}
      <div className="chat-scroll" style={showList ? { display: 'none' } : undefined}>
      <div
        className="chat-messages"
        ref={scroller}
        onScroll={onScroll}
        onWheel={(e) => e.deltaY < 0 && leaveBottom()}
        onTouchMove={leaveBottom}
        onKeyDown={(e) => ['ArrowUp', 'PageUp', 'Home'].includes(e.key) && leaveBottom()}
      >
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
              {m.notices?.map((n) => (
                <div key={n} className="callout warn small" style={{ marginBottom: 6 }}>
                  <AlertTriangle size={14} />
                  <span>{noticeText(n)}</span>
                </div>
              ))}
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
      {!atBottom && messages.length > 0 && (
        <button className="scroll-bottom" title={t('Scroll to the end')} onClick={scrollToBottom}>
          <ArrowDown size={15} />
        </button>
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

