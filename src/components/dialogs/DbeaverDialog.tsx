// Imports connections from DBeaver. Passwords never reach the webview: the preview only shows
// whether one exists, the backend copies it straight into the system keychain.
import { useEffect, useState } from 'react'
import { AlertTriangle, FolderOpen, Import, KeyRound, Loader2, Lock, LockOpen, Network } from 'lucide-react'
import type { DbeaverCandidate, DbeaverScan } from '@shared/types'
import { api, errorMessage } from '@/lib/api'
import { t } from '@/lib/i18n'
import { useStore } from '@/lib/store'
import { DbIcon, Modal } from '../ui'

const key = (c: DbeaverCandidate) => `${c.source}\n${c.id}`

function warningText(code: string): string {
  switch (code) {
    case 'tls-unverified':
      return t('DBeaver did not verify the server certificate. SQLighter does - if the connection fails, set the CA file or pin the certificate.')
    case 'oracle-sid':
      return t('Oracle SID: SQLighter connects by service name - check the value.')
    case 'ssh-jump':
      return t('SSH jump hosts are not supported - only the first tunnel host is imported.')
    case 'proxy':
      return t('Proxy settings are not imported.')
    case 'auth-model':
      return t('DBeaver uses a special authentication method here - user and password may have to be entered manually.')
    default:
      return code
  }
}

function target(c: DbeaverCandidate): string {
  const cfg = c.config
  if (cfg.type === 'sqlite') return cfg.filePath ?? ''
  const db = cfg.database || cfg.serviceName || ''
  return `${cfg.user ? cfg.user + '@' : ''}${cfg.host}:${cfg.port}${db ? '/' + db : ''}`
}

export function DbeaverDialog() {
  const { setDialog, toast, loadTree } = useStore.getState()
  const [scan, setScan] = useState<DbeaverScan | null>(null)
  const [sel, setSel] = useState<Set<string>>(new Set())
  const [passwords, setPasswords] = useState(true)
  const [allowInsecure, setAllowInsecure] = useState(false)
  const [busy, setBusy] = useState<'scan' | 'import' | null>('scan')

  const load = async (path?: string) => {
    setBusy('scan')
    try {
      const r = await api.dbeaverScan(path)
      setScan(r)
      setSel(new Set(r.connections.filter((c) => !c.exists).map(key)))
    } catch (e) {
      toast(errorMessage(e), 'error')
    } finally {
      setBusy(null)
    }
  }

  useEffect(() => {
    load()
  }, [])

  const pick = async () => {
    const p = await api.pickOpenFile([{ name: 'DBeaver data-sources.json', extensions: ['json'] }])
    if (p) load(p)
  }

  const run = async () => {
    if (!scan) return
    setBusy('import')
    try {
      const chosen = scan.connections.filter((c) => sel.has(key(c))).map((c) => ({ source: c.source, id: c.id }))
      const n = await api.dbeaverImport(chosen, { passwords, allowInsecure })
      await loadTree()
      setDialog(null)
      toast(t('{count} connections imported', { count: String(n) }), 'success')
    } catch (e) {
      toast(errorMessage(e), 'error')
      setBusy(null)
    }
  }

  const conns = scan?.connections ?? []
  const chosen = conns.filter((c) => sel.has(key(c)))
  const anyUnencrypted = chosen.some((c) => c.unencrypted)

  return (
    <Modal
      title={t('Import from DBeaver')}
      icon={<Import size={16} />}
      size="wide"
      onClose={() => setDialog(null)}
      footer={
        <>
          <button className="btn ghost" onClick={pick} disabled={!!busy}>
            <FolderOpen size={14} /> {t('Choose file…')}
          </button>
          <span className="grow" />
          <button className="btn" onClick={() => setDialog(null)}>
            {t('Cancel')}
          </button>
          <button className="btn primary" onClick={run} disabled={!!busy || !chosen.length}>
            {busy === 'import' && <Loader2 size={14} className="spin" />} {t('Import {count}', { count: String(chosen.length) })}
          </button>
        </>
      }
    >
      <div className="col" style={{ gap: 12 }}>
        {busy === 'scan' && (
          <div className="row small muted">
            <Loader2 size={14} className="spin" /> {t('Searching for DBeaver connections…')}
          </div>
        )}
        {scan && !scan.sources.length && (
          <div className="callout warn">
            <AlertTriangle size={16} />
            <span>{t('No DBeaver workspace found. Choose the file data-sources.json manually (workspace6/General/.dbeaver/).')}</span>
          </div>
        )}
        {scan?.errors.map((e) => (
          <div key={e} className="callout danger">
            <AlertTriangle size={16} color="var(--danger)" />
            <span className="mono small" style={{ wordBreak: 'break-word' }}>
              {e}
            </span>
          </div>
        ))}
        {scan && scan.sources.length > 0 && (
          <div className="hint" style={{ wordBreak: 'break-all' }}>
            {scan.sources.join(' · ')}
          </div>
        )}

        {conns.length > 0 && (
          <>
            <div className="row">
              <span className="label grow">{t('{count} connections found', { count: String(conns.length) })}</span>
              <button className="btn small ghost" onClick={() => setSel(new Set(conns.map(key)))}>
                {t('All')}
              </button>
              <button className="btn small ghost" onClick={() => setSel(new Set())}>
                {t('None')}
              </button>
            </div>
            <div className="list-box" style={{ maxHeight: 340 }}>
              {conns.map((c) => (
                <label key={key(c)} className="check" style={{ alignItems: 'flex-start' }}>
                  <input
                    type="checkbox"
                    checked={sel.has(key(c))}
                    onChange={(e) => {
                      const n = new Set(sel)
                      if (e.target.checked) n.add(key(c))
                      else n.delete(key(c))
                      setSel(n)
                    }}
                  />
                  <DbIcon type={c.config.type} size={18} />
                  <span className="grow" style={{ minWidth: 0 }}>
                    <span className="row" style={{ gap: 6, flexWrap: 'wrap' }}>
                      <b>{c.config.name}</b>
                      {c.folder.length > 0 && <span className="muted small">{c.folder.join(' / ')}</span>}
                      {c.exists && <span className="badge">{t('already exists')}</span>}
                      {c.config.production && <span className="badge danger">{t('Production')}</span>}
                    </span>
                    <span className="mono small muted ellipsis" style={{ display: 'block' }}>
                      {target(c)}
                    </span>
                    <span className="row small" style={{ gap: 10, marginTop: 2, flexWrap: 'wrap' }}>
                      {c.config.tls.mode !== 'disabled' ? (
                        <span className="row" style={{ gap: 4, color: 'var(--ok)' }}>
                          <Lock size={12} /> TLS
                        </span>
                      ) : c.unencrypted ? (
                        <span className="row" style={{ gap: 4, color: 'var(--danger)' }}>
                          <LockOpen size={12} /> {t('unencrypted')}
                        </span>
                      ) : null}
                      {c.config.ssh.enabled && (
                        <span className="row" style={{ gap: 4, color: 'var(--ok)' }}>
                          <Network size={12} /> SSH {c.config.ssh.host}
                        </span>
                      )}
                      {(c.hasPassword || c.hasSshSecret) && (
                        <span className="row muted" style={{ gap: 4 }}>
                          <KeyRound size={12} /> {t('password stored')}
                        </span>
                      )}
                    </span>
                    {c.warnings.map((w) => (
                      <span key={w} className="row small" style={{ gap: 4, color: 'var(--warn)', marginTop: 2 }}>
                        <AlertTriangle size={12} /> {warningText(w)}
                      </span>
                    ))}
                  </span>
                </label>
              ))}
            </div>
          </>
        )}
        {scan && scan.sources.length > 0 && !conns.length && !busy && <div className="hint">{t('No importable connections found.')}</div>}
        {scan && scan.skipped.length > 0 && (
          <div className="hint">
            {t('Not supported and skipped: {names}', { names: scan.skipped.map((x) => `${x.name} (${x.driver})`).join(', ') })}
          </div>
        )}

        {conns.length > 0 && (
          <>
            <label className="check">
              <input type="checkbox" checked={passwords} onChange={(e) => setPasswords(e.target.checked)} />
              <span>
                {t('Import saved passwords')}
                <div className="hint">{t('They are stored in the system keychain and are never shown in the app.')}</div>
              </span>
            </label>
            {anyUnencrypted && (
              <label className="check callout danger">
                <input type="checkbox" checked={allowInsecure} onChange={(e) => setAllowInsecure(e.target.checked)} />
                <span>
                  {t('Allow unencrypted connections to remote hosts, as configured in DBeaver')}
                  <div className="hint">
                    {t('Passwords and data are then sent in plain text. Better: enable TLS or an SSH tunnel for these connections after the import.')}
                  </div>
                </span>
              </label>
            )}
          </>
        )}
      </div>
    </Modal>
  )
}
