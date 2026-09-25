// Prompt, confirm, value viewer, history, SSH host key and approval dialogs.
import { useEffect, useMemo, useState } from 'react'
import { AlertTriangle, CheckCircle2, ClipboardCopy, History, KeyRound, ShieldAlert, XCircle } from 'lucide-react'
import type { ApprovalRequest, HistoryEntry, HostKeyPrompt } from '@shared/types'
import { api, errorMessage } from '@/lib/api'
import { copyText, formatDuration, prettyValue } from '@/lib/format'
import { t } from '@/lib/i18n'
import { useStore } from '@/lib/store'
import { Logo, Modal } from '../ui'

export function PromptDialog(props: { title: string; label: string; value?: string; password?: boolean; onSubmit: (v: string) => void | Promise<void> }) {
  const { setDialog, toast } = useStore.getState()
  const [v, setV] = useState(props.value ?? '')
  const submit = async () => {
    try {
      setDialog(null)
      await props.onSubmit(v)
    } catch (e) {
      toast(errorMessage(e), 'error')
    }
  }
  return (
    <Modal
      title={props.title}
      size="narrow"
      onClose={() => setDialog(null)}
      footer={
        <>
          <button className="btn" onClick={() => setDialog(null)}>
            {t('Cancel')}
          </button>
          <button className="btn primary" onClick={submit} disabled={!props.password && !v.trim()}>
            {t('OK')}
          </button>
        </>
      }
    >
      <div className="field">
        <label>{props.label}</label>
        <input
          className="input"
          autoFocus
          type={props.password ? 'password' : 'text'}
          value={v}
          onChange={(e) => setV(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && submit()}
        />
      </div>
    </Modal>
  )
}

export function ConfirmDialog(props: { title: string; message: string; details?: string; danger?: boolean; confirmLabel?: string; onConfirm: () => void | Promise<void> }) {
  const { setDialog, toast } = useStore.getState()
  return (
    <Modal
      title={props.title}
      icon={props.danger ? <AlertTriangle size={18} color="var(--danger)" /> : undefined}
      onClose={() => setDialog(null)}
      footer={
        <>
          <button className="btn" onClick={() => setDialog(null)} autoFocus={!!props.danger}>
            {t('Cancel')}
          </button>
          <button
            className={`btn ${props.danger ? 'danger' : 'primary'}`}
            autoFocus={!props.danger}
            onClick={async () => {
              setDialog(null)
              try {
                await props.onConfirm()
              } catch (e) {
                toast(errorMessage(e), 'error')
              }
            }}
          >
            {props.confirmLabel ?? t('OK')}
          </button>
        </>
      }
    >
      <div style={{ lineHeight: 1.5 }}>{props.message}</div>
      {props.details && (
        <pre className="fingerprint" style={{ maxHeight: 320, overflow: 'auto', whiteSpace: 'pre-wrap', margin: 0 }}>
          {props.details}
        </pre>
      )}
    </Modal>
  )
}

export function ValueDialog({ title, value }: { title: string; value: string }) {
  const { setDialog, toast } = useStore.getState()
  const pretty = useMemo(() => prettyValue(value), [value])
  return (
    <Modal
      title={title}
      onClose={() => setDialog(null)}
      footer={
        <button className="btn" onClick={() => copyText(value).then(() => toast(t('Copied to clipboard'), 'success'))}>
          <ClipboardCopy size={14} /> {t('Copy')}
        </button>
      }
    >
      <textarea className="textarea value-view" readOnly value={pretty} />
      <span className="hint">{t('{n} characters', { n: value.length })}</span>
    </Modal>
  )
}

export function HistoryDialog({ connectionId }: { connectionId?: string }) {
  const { setDialog, openSqlTab } = useStore.getState()
  const connections = useStore((s) => s.tree.connections)
  const [items, setItems] = useState<HistoryEntry[]>([])
  const [q, setQ] = useState('')
  const [onlyThis, setOnlyThis] = useState(!!connectionId)
  useEffect(() => {
    api.getHistory(onlyThis ? connectionId : undefined).then(setItems)
  }, [connectionId, onlyThis])
  const filtered = items.filter((i) => !q || i.sql.toLowerCase().includes(q.toLowerCase()))
  return (
    <Modal title={t('Query history')} icon={<History size={18} />} size="wide" onClose={() => setDialog(null)}>
      <div className="row">
        <input className="input" autoFocus placeholder={t('Search…')} value={q} onChange={(e) => setQ(e.target.value)} />
        {connectionId && (
          <label className="check small" style={{ whiteSpace: 'nowrap' }}>
            <input type="checkbox" checked={onlyThis} onChange={(e) => setOnlyThis(e.target.checked)} /> {t('This connection only')}
          </label>
        )}
      </div>
      <div className="list-box" style={{ maxHeight: 460 }}>
        {filtered.map((h) => (
          <div
            key={h.id}
            className="tree-row"
            style={{ height: 'auto', padding: '8px 10px', alignItems: 'flex-start', borderBottom: '1px solid var(--border)' }}
            onClick={() => {
              openSqlTab({ connectionId: h.connectionId, sql: h.sql })
              setDialog(null)
            }}
          >
            {h.ok ? <CheckCircle2 size={14} color="var(--ok)" /> : <XCircle size={14} color="var(--danger)" />}
            <div className="col grow" style={{ gap: 3 }}>
              <span className="mono small" style={{ whiteSpace: 'pre-wrap', maxHeight: 60, overflow: 'hidden' }}>
                {h.sql}
              </span>
              <span className="small muted">
                {new Date(h.at).toLocaleString()} · {connections.find((c) => c.id === h.connectionId)?.name ?? '?'} · {formatDuration(h.durationMs)}
              </span>
            </div>
          </div>
        ))}
        {!filtered.length && <div className="hint" style={{ padding: 16 }}>{t('No entries')}</div>}
      </div>
    </Modal>
  )
}

export function HostKeyDialog({ prompt, onDone }: { prompt: HostKeyPrompt; onDone: () => void }) {
  const changed = !!prompt.previous
  const [ack, setAck] = useState(false)
  const answer = (accept: boolean) => {
    api.answerHostKey(prompt.promptId, accept)
    onDone()
  }
  return (
    <Modal
      title={changed ? t('WARNING: SSH host key has changed!') : t('Unknown SSH host')}
      icon={changed ? <ShieldAlert size={20} color="var(--danger)" /> : <KeyRound size={18} />}
      onClose={() => answer(false)}
      footer={
        <>
          <button className="btn" onClick={() => answer(false)} autoFocus>
            {t('Reject')}
          </button>
          <button className={`btn ${changed ? 'danger' : 'primary'}`} disabled={changed && !ack} onClick={() => answer(true)}>
            {changed ? t('Trust new key') : t('Trust and connect')}
          </button>
        </>
      }
    >
      {changed ? (
        <div className="callout danger">
          <ShieldAlert size={18} color="var(--danger)" />
          <span>
            {t('The server {host} presented a different host key than before. Someone could be intercepting the connection (man-in-the-middle attack), or the server was reinstalled. Only continue if the administrator confirmed the new key.', {
              host: `${prompt.host}:${prompt.port}`
            })}
          </span>
        </div>
      ) : (
        <div>{t('The authenticity of {host} cannot be established. Compare the fingerprint with the one of the server (ssh-keygen -lf /etc/ssh/ssh_host_*_key.pub).', { host: `${prompt.host}:${prompt.port}` })}</div>
      )}
      <div className="kv">
        <span>{t('Algorithm')}</span>
        <span className="mono">{prompt.algorithm}</span>
      </div>
      <div className="fingerprint">{prompt.fingerprint}</div>
      {changed && prompt.previous && (
        <>
          <span className="small muted">{t('Previously trusted')}:</span>
          <div className="fingerprint" style={{ opacity: 0.7 }}>
            {prompt.previous}
          </div>
          <label className="check">
            <input type="checkbox" checked={ack} onChange={(e) => setAck(e.target.checked)} />
            <span>{t('I verified the new fingerprint with the server administrator.')}</span>
          </label>
        </>
      )}
    </Modal>
  )
}

export function ApprovalDialog({ req, onDone }: { req: ApprovalRequest; onDone: () => void }) {
  const answer = (ok: boolean) => {
    api.answerApproval(req.approvalId, ok)
    onDone()
  }
  return (
    <Modal
      title={t('Approve SQL execution?')}
      icon={<ShieldAlert size={18} color="var(--warn)" />}
      onClose={() => answer(false)}
      footer={
        <>
          <button className="btn" onClick={() => answer(false)} autoFocus>
            {t('Reject')}
          </button>
          <button className="btn danger" onClick={() => answer(true)}>
            {t('Execute')}
          </button>
        </>
      }
    >
      <div>{t('{source} wants to execute the following SQL on "{name}":', { source: req.source, name: req.connectionName })}</div>
      <pre className="fingerprint" style={{ maxHeight: 360, overflow: 'auto', whiteSpace: 'pre-wrap', margin: 0 }}>
        {req.sql}
      </pre>
    </Modal>
  )
}

export function AboutDialog() {
  const { setDialog } = useStore.getState()
  return (
    <Modal title="SQLighter" size="narrow" onClose={() => setDialog(null)}>
      <div className="col" style={{ alignItems: 'center', textAlign: 'center', gap: 10 }}>
        <Logo size={56} />
        <b>SQLighter 0.1.0</b>
        <span className="hint">{t('A modern, minimal and secure SQL client with an AI assistant.')}</span>
        <span className="hint">PostgreSQL · MySQL · MariaDB · SQLite · SQL Server · Oracle · CockroachDB</span>
      </div>
    </Modal>
  )
}

export function PasswordPrompt({ title, label, onDone }: { title: string; label: string; onDone: (pw: string | null) => void }) {
  const [v, setV] = useState('')
  return (
    <Modal
      title={title}
      size="narrow"
      icon={<KeyRound size={18} />}
      onClose={() => onDone(null)}
      footer={
        <>
          <button className="btn" onClick={() => onDone(null)}>
            {t('Cancel')}
          </button>
          <button className="btn primary" onClick={() => onDone(v)}>
            {t('Connect')}
          </button>
        </>
      }
    >
      <div className="field">
        <label>{label}</label>
        <input className="input" autoFocus type="password" value={v} onChange={(e) => setV(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && onDone(v)} />
      </div>
    </Modal>
  )
}
