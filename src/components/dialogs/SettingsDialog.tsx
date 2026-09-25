// Settings: general, AI providers, MCP server, security.
import { useEffect, useState } from 'react'
import { Bot, ClipboardCopy, KeyRound, Loader2, Plug, Plus, RefreshCw, Settings2, Shield, Trash2 } from 'lucide-react'
import type { AiAccessLevel, AiProviderKind, AiProviderView, McpInfo, Settings, SettingsView } from '@shared/types'
import { api, errorMessage } from '@/lib/api'
import { copyText } from '@/lib/format'
import { setLanguage, t } from '@/lib/i18n'
import { uid, useStore } from '@/lib/store'
import { Modal } from '../ui'

type Section = 'general' | 'ai' | 'mcp' | 'security'

const KIND_DEFAULTS: Record<AiProviderKind, { name: string; baseUrl: string; model: string }> = {
  ollama: { name: 'Ollama', baseUrl: 'http://127.0.0.1:11434', model: 'qwen2.5-coder:7b' },
  openai: { name: 'OpenAI-compatible', baseUrl: 'https://api.openai.com/v1', model: 'gpt-4.1-mini' },
  anthropic: { name: 'Claude (Anthropic API)', baseUrl: 'https://api.anthropic.com', model: 'claude-sonnet-5' },
  'claude-code': { name: 'Claude Code', baseUrl: '', model: '' }
}

function toSettings(v: SettingsView): Settings {
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const { secureStorage, aiProviders, ...rest } = v
  return { ...rest, aiProviders: aiProviders.map(({ hasApiKey: _h, ...p }) => p) }
}

export function SettingsDialog({ section = 'general' }: { section?: Section }) {
  const { setDialog, toast } = useStore.getState()
  const view = useStore((s) => s.settings)
  const [sec, setSec] = useState<Section>(section)
  const [s, setS] = useState<SettingsView | null>(view)
  const [keys, setKeys] = useState<Record<string, string>>({})
  const [saving, setSaving] = useState(false)

  if (!s) return null
  const set = (patch: Partial<SettingsView>) => setS({ ...s, ...patch })

  const save = async (close = true) => {
    setSaving(true)
    try {
      for (const [id, k] of Object.entries(keys)) await api.setAiApiKey(id, k || null)
      const v = await api.saveSettings(toSettings(s))
      useStore.setState({ settings: v })
      setLanguage(v.language)
      applyTheme(v.theme)
      setKeys({})
      if (close) setDialog(null)
      else setS(v)
      toast(t('Settings saved'), 'success')
    } catch (e) {
      toast(errorMessage(e), 'error')
    } finally {
      setSaving(false)
    }
  }

  const nav: { key: Section; label: string; icon: React.ReactNode }[] = [
    { key: 'general', label: t('General'), icon: <Settings2 size={14} /> },
    { key: 'ai', label: t('AI assistant'), icon: <Bot size={14} /> },
    { key: 'mcp', label: 'MCP · Claude Code / Cowork', icon: <Plug size={14} /> },
    { key: 'security', label: t('Security'), icon: <Shield size={14} /> }
  ]

  return (
    <Modal
      title={t('Settings')}
      onClose={() => setDialog(null)}
      size="wide"
      tabs={
        <div className="modal-tabs">
          {nav.map((n) => (
            <button key={n.key} className={sec === n.key ? 'on' : ''} onClick={() => setSec(n.key)}>
              {n.icon} {n.label}
            </button>
          ))}
        </div>
      }
      footer={
        <>
          <button className="btn" onClick={() => setDialog(null)}>
            {t('Cancel')}
          </button>
          <button className="btn primary" onClick={() => save()} disabled={saving}>
            {t('Save')}
          </button>
        </>
      }
    >
      {sec === 'general' && (
        <>
          <div className="grid2">
            <div className="field">
              <label>{t('Language')}</label>
              <select className="select" value={s.language} onChange={(e) => set({ language: e.target.value as SettingsView['language'] })}>
                <option value="system">{t('System')}</option>
                <option value="de">Deutsch</option>
                <option value="en">English</option>
              </select>
            </div>
            <div className="field">
              <label>{t('Theme')}</label>
              <select className="select" value={s.theme} onChange={(e) => set({ theme: e.target.value as SettingsView['theme'] })}>
                <option value="system">{t('System')}</option>
                <option value="dark">{t('Dark')}</option>
                <option value="light">{t('Light')}</option>
              </select>
            </div>
            <div className="field">
              <label>{t('Editor font size')}</label>
              <input className="input" type="number" min={10} max={24} value={s.editorFontSize} onChange={(e) => set({ editorFontSize: Number(e.target.value) || 13 })} />
            </div>
            <div className="field">
              <label>{t('Max. rows per result')}</label>
              <input className="input" type="number" min={10} max={1000000} value={s.maxRows} onChange={(e) => set({ maxRows: Number(e.target.value) || 1000 })} />
            </div>
            <div className="field">
              <label>{t('Query timeout (seconds, 0 = none)')}</label>
              <input className="input" type="number" min={0} value={s.queryTimeoutSec} onChange={(e) => set({ queryTimeoutSec: Number(e.target.value) || 0 })} />
            </div>
          </div>
          <label className="check">
            <input type="checkbox" checked={s.confirmDestructive} onChange={(e) => set({ confirmDestructive: e.target.checked })} />
            <span>
              {t('Confirm destructive statements')}
              <div className="hint">{t('DROP, TRUNCATE and DELETE/UPDATE without WHERE ask before running.')}</div>
            </span>
          </label>
          <label className="check">
            <input type="checkbox" checked={s.autoCommit} onChange={(e) => set({ autoCommit: e.target.checked })} />
            <span>
              {t('Auto-commit by default')}
              <div className="hint">{t('Without auto-commit, changes stay in a transaction until you click Commit.')}</div>
            </span>
          </label>
          <div className="row">
            <button
              className="btn small"
              onClick={() =>
                setDialog({
                  type: 'confirm',
                  title: t('Clear query history?'),
                  message: t('All entries of the query history are deleted.'),
                  danger: true,
                  onConfirm: async () => {
                    await api.clearHistory()
                    toast(t('History cleared'), 'success')
                  }
                })
              }
            >
              <Trash2 size={13} /> {t('Clear query history')}
            </button>
          </div>
        </>
      )}

      {sec === 'ai' && <AiSection s={s} set={set} keys={keys} setKeys={setKeys} />}
      {sec === 'mcp' && <McpSection s={s} set={set} saveNow={() => save(false)} />}
      {sec === 'security' && (
        <>
          <div className={`callout ${s.secureStorage.available ? 'ok' : 'warn'}`}>
            <KeyRound size={16} />
            <span>
              <b>{t('Credential storage')}: </b>
              {s.secureStorage.backend}
              <div className="hint">
                {s.secureStorage.available
                  ? t('Passwords, SSH passphrases and API keys are stored in the operating system keychain, never in configuration files.')
                  : t('No system keychain is available. Secrets are kept in memory for this session only and must be re-entered after a restart.')}
              </div>
            </span>
          </div>
          <div className="section-title">{t('Built-in protections')}</div>
          <ul className="hint" style={{ margin: 0, paddingLeft: 18, lineHeight: 1.7 }}>
            <li>{t('TLS always verifies the server (full verification, CA verification or a pinned certificate). There is no insecure "encrypt only" mode.')}</li>
            <li>{t('Unencrypted connections to remote hosts are refused unless you explicitly accept the risk per connection.')}</li>
            <li>{t('SSH host keys are verified (known_hosts / confirmed fingerprint). A changed key blocks the connection (possible man-in-the-middle).')}</li>
            <li>{t('AI providers must use HTTPS; plain HTTP is only allowed for localhost (e.g. Ollama) or an explicit per-provider exception.')}</li>
            <li>{t('AI agents never execute data-modifying SQL. Read-only queries (if enabled) run in a READ ONLY transaction that is rolled back.')}</li>
            <li>{t('The user interface can only read or write files you picked in a file dialog; it cannot load remote content (strict Content Security Policy).')}</li>
            <li>{t('Configuration files are written atomically with owner-only permissions.')}</li>
          </ul>
        </>
      )}
    </Modal>
  )
}

function AiSection({ s, set, keys, setKeys }: { s: SettingsView; set: (p: Partial<SettingsView>) => void; keys: Record<string, string>; setKeys: (k: Record<string, string>) => void }) {
  const [editing, setEditing] = useState<string | null>(s.aiProviders[0]?.id ?? null)
  const [models, setModels] = useState<Record<string, string[]>>({})
  const [loading, setLoading] = useState<string | null>(null)
  const { toast } = useStore.getState()
  const p = s.aiProviders.find((x) => x.id === editing)
  const updateP = (patch: Partial<AiProviderView>) => set({ aiProviders: s.aiProviders.map((x) => (x.id === editing ? { ...x, ...patch } : x)) })

  const add = (kind: AiProviderKind) => {
    const d = KIND_DEFAULTS[kind]
    const np: AiProviderView = { id: uid(), kind, name: d.name, baseUrl: d.baseUrl, model: d.model, hasApiKey: false, temperature: 0.2 }
    set({ aiProviders: [...s.aiProviders, np] })
    setEditing(np.id)
  }

  const loadModels = async () => {
    if (!p) return
    setLoading('models')
    try {
      // Model listing uses the saved configuration; save pending key/url changes first.
      if (keys[p.id]) await api.setAiApiKey(p.id, keys[p.id])
      await api.saveSettings(toSettings(s))
      setModels({ ...models, [p.id]: await api.aiListModels(p.id) })
    } catch (e) {
      toast(errorMessage(e), 'error')
    } finally {
      setLoading(null)
    }
  }

  const detect = async () => {
    if (!p) return
    setLoading('detect')
    try {
      const info = await api.detectClaude(p.command)
      updateP({ command: info.path })
      toast(`${info.path} · ${info.version}`, 'success')
    } catch (e) {
      toast(errorMessage(e), 'error')
    } finally {
      setLoading(null)
    }
  }

  const access: { v: AiAccessLevel; label: string; desc: string }[] = [
    { v: 'none', label: t('No database access'), desc: t('The AI only sees what you write. Nothing about your databases is sent.') },
    { v: 'schema', label: t('Schema (recommended)'), desc: t('Table and column names/types are shared so the AI can write correct SQL. No row data.') },
    { v: 'read', label: t('Schema + read-only queries'), desc: t('The AI may run single SELECT statements (max. 50 rows) in a READ ONLY transaction. Row data is sent to the provider.') }
  ]

  return (
    <>
      <div className="section-title">{t('Database access for AI agents')}</div>
      <div className="col" style={{ gap: 6 }}>
        {access.map((a) => (
          <label key={a.v} className="check" style={{ padding: '7px 10px', borderRadius: 8, border: `1px solid ${s.aiAccess === a.v ? 'var(--accent)' : 'var(--border)'}` }}>
            <input type="radio" checked={s.aiAccess === a.v} onChange={() => set({ aiAccess: a.v })} />
            <span>
              <b>{a.label}</b>
              <div className="hint">{a.desc}</div>
            </span>
          </label>
        ))}
      </div>
      <div className="section-title">{t('Providers')}</div>
      <div className="row" style={{ alignItems: 'stretch', gap: 14 }}>
        <div className="col" style={{ width: 210, flex: 'none', gap: 4 }}>
          {s.aiProviders.map((x) => (
            <div
              key={x.id}
              className={`tree-row ${editing === x.id ? 'selected' : ''}`}
              style={{ borderRadius: 6, paddingLeft: 8 }}
              onClick={() => setEditing(x.id)}
            >
              <Bot size={14} />
              <span className="tlabel">{x.name}</span>
              {s.aiDefaultProviderId === x.id && <span className="badge accent">{t('default')}</span>}
            </div>
          ))}
          <select
            className="select sm"
            value=""
            onChange={(e) => e.target.value && add(e.target.value as AiProviderKind)}
            style={{ marginTop: 6 }}
          >
            <option value="">{t('+ Add provider…')}</option>
            <option value="ollama">Ollama</option>
            <option value="openai">{t('OpenAI-compatible (OpenAI, LM Studio, OpenRouter, vLLM…)')}</option>
            <option value="anthropic">Claude (Anthropic API)</option>
            <option value="claude-code">Claude Code (CLI)</option>
          </select>
        </div>
        {p && (
          <div className="col grow" style={{ gap: 12 }}>
            <div className="grid2">
              <div className="field">
                <label>{t('Name')}</label>
                <input className="input" value={p.name} onChange={(e) => updateP({ name: e.target.value })} />
              </div>
              <div className="field">
                <label>{t('Type')}</label>
                <input className="input" disabled value={p.kind} />
              </div>
            </div>
            {p.kind === 'claude-code' ? (
              <>
                <div className="callout">
                  <Bot size={16} />
                  <span>
                    {t('Uses your local Claude Code installation (subscription or API key). Claude Code runs without shell or file tools; it can only inspect the database through SQLighter (within the access level above).')}
                  </span>
                </div>
                <div className="field">
                  <label>{t('claude executable')}</label>
                  <div className="row">
                    <input className="input mono" placeholder={t('auto-detect')} value={p.command ?? ''} onChange={(e) => updateP({ command: e.target.value || undefined })} />
                    <button className="btn" onClick={detect} disabled={!!loading}>
                      {loading === 'detect' && <Loader2 size={14} className="spin" />} {t('Detect')}
                    </button>
                  </div>
                </div>
                <div className="field">
                  <label>{t('Model (optional)')}</label>
                  <input className="input mono" placeholder="sonnet / opus / haiku" value={p.model} onChange={(e) => updateP({ model: e.target.value })} />
                </div>
              </>
            ) : (
              <>
                <div className="field">
                  <label>{t('Base URL')}</label>
                  <input className="input mono" value={p.baseUrl} onChange={(e) => updateP({ baseUrl: e.target.value.trim() })} />
                </div>
                <div className="grid2">
                  <div className="field">
                    <label>{t('Model')}</label>
                    <div className="row">
                      <input className="input mono" list={`models-${p.id}`} value={p.model} onChange={(e) => updateP({ model: e.target.value })} />
                      <datalist id={`models-${p.id}`}>
                        {(models[p.id] ?? []).map((m) => (
                          <option key={m} value={m} />
                        ))}
                      </datalist>
                      <button className="icon-btn" title={t('Load available models')} onClick={loadModels}>
                        <RefreshCw size={14} className={loading === 'models' ? 'spin' : ''} />
                      </button>
                    </div>
                  </div>
                  <div className="field">
                    <label>{t('Temperature')}</label>
                    <input className="input" type="number" step={0.1} min={0} max={2} value={p.temperature ?? ''} onChange={(e) => updateP({ temperature: e.target.value === '' ? undefined : Number(e.target.value) })} />
                  </div>
                </div>
                {p.kind !== 'ollama' && (
                  <div className="field">
                    <label>{t('API key')}</label>
                    <input
                      className="input mono"
                      type="password"
                      autoComplete="off"
                      placeholder={p.hasApiKey ? t('•••••• stored in keychain (enter to replace)') : 'sk-…'}
                      value={keys[p.id] ?? ''}
                      onChange={(e) => setKeys({ ...keys, [p.id]: e.target.value })}
                    />
                    {p.hasApiKey && (
                      <button className="btn small" style={{ alignSelf: 'flex-start' }} onClick={() => setKeys({ ...keys, [p.id]: '' })}>
                        {t('Remove stored key')}
                      </button>
                    )}
                  </div>
                )}
                <label className="check">
                  <input type="checkbox" checked={!!p.allowInsecureHttp} onChange={(e) => updateP({ allowInsecureHttp: e.target.checked })} />
                  <span>
                    {t('Allow unencrypted http:// for a non-local host')}
                    <div className="hint">{t('Only for trusted networks (e.g. Ollama on another machine in the LAN). Prompts and schema would travel unencrypted.')}</div>
                  </span>
                </label>
              </>
            )}
            <div className="row">
              <button className="btn small" onClick={() => set({ aiDefaultProviderId: p.id })} disabled={s.aiDefaultProviderId === p.id}>
                {t('Use as default')}
              </button>
              <div className="grow" />
              <button
                className="btn small"
                onClick={() => {
                  set({ aiProviders: s.aiProviders.filter((x) => x.id !== p.id) })
                  setEditing(s.aiProviders.find((x) => x.id !== p.id)?.id ?? null)
                }}
              >
                <Trash2 size={13} /> {t('Remove')}
              </button>
            </div>
          </div>
        )}
        {!p && (
          <button className="btn" onClick={() => add('ollama')}>
            <Plus size={14} /> {t('Add provider')}
          </button>
        )}
      </div>
    </>
  )
}

function McpSection({ s, set, saveNow }: { s: SettingsView; set: (p: Partial<SettingsView>) => void; saveNow: () => Promise<void> }) {
  const [info, setInfo] = useState<McpInfo | null>(null)
  const { toast } = useStore.getState()
  useEffect(() => {
    api.mcpInfo().then(setInfo).catch(() => undefined)
  }, [s.mcpEnabled])
  const copy = (text: string) => copyText(text).then(() => toast(t('Copied to clipboard'), 'success'))
  return (
    <>
      <div className="callout">
        <Plug size={16} />
        <span>
          {t('The MCP server lets Claude Code, Claude Desktop / Cowork and other MCP clients use your SQLighter connections: list tables, describe them, open SQL in an editor tab and (depending on the access level) run read-only queries.')}
        </span>
      </div>
      <label className="check">
        <input type="checkbox" checked={s.mcpEnabled} onChange={(e) => set({ mcpEnabled: e.target.checked })} />
        <span>
          <b>{t('Enable MCP server')}</b>
          <div className="hint">{t('Listens on 127.0.0.1 only and requires a secret token. Save to apply.')}</div>
        </span>
      </label>
      <div className="grid2">
        <div className="field">
          <label>{t('Port')}</label>
          <input className="input mono" type="number" value={s.mcpPort} onChange={(e) => set({ mcpPort: Number(e.target.value) || 47821 })} />
        </div>
      </div>
      <label className="check">
        <input type="checkbox" checked={s.mcpAllowWrite} onChange={(e) => set({ mcpAllowWrite: e.target.checked })} />
        <span>
          {t('Allow MCP clients to request data-modifying SQL')}
          <div className="hint">{t('Every such statement is shown to you in SQLighter and only runs after you approve it.')}</div>
        </span>
      </label>
      {info?.enabled && info.token ? (
        <>
          <div className="section-title">Claude Code</div>
          <div className="row">
            <div className="fingerprint grow">{info.claudeCodeCommand.replace(info.token, '••••••••')}</div>
            <button className="icon-btn" title={t('Copy')} onClick={() => copy(info.claudeCodeCommand)}>
              <ClipboardCopy size={15} />
            </button>
          </div>
          <div className="section-title">Claude Desktop / Cowork</div>
          <span className="hint">{t('Add this to the Claude Desktop configuration (Settings → Developer → Edit config). Requires Node.js.')}</span>
          <div className="row" style={{ alignItems: 'flex-start' }}>
            <pre className="fingerprint grow" style={{ margin: 0, whiteSpace: 'pre-wrap' }}>
              {info.claudeDesktopConfig}
            </pre>
            <button className="icon-btn" title={t('Copy')} onClick={() => copy(info.claudeDesktopConfig)}>
              <ClipboardCopy size={15} />
            </button>
          </div>
          <div className="row">
            <span className="small muted grow">
              {info.running ? t('Running on {url}', { url: info.url }) : t('Not running (save settings to start)')}
            </span>
            <button
              className="btn small"
              onClick={async () => {
                setInfo(await api.mcpRegenerateToken())
                toast(t('New token created. Update your MCP client configuration.'), 'info')
              }}
            >
              <RefreshCw size={13} /> {t('New token')}
            </button>
          </div>
        </>
      ) : (
        s.mcpEnabled && (
          <button className="btn" style={{ alignSelf: 'flex-start' }} onClick={() => saveNow().then(() => api.mcpInfo().then(setInfo))}>
            {t('Save and show configuration')}
          </button>
        )
      )}
    </>
  )
}

export function applyTheme(theme: string) {
  const dark = theme === 'dark' || (theme === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches)
  document.documentElement.dataset.theme = dark ? 'dark' : 'light'
}
