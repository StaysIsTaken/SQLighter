// Create / edit a connection.
import { useMemo, useState } from 'react'
import { AlertTriangle, CheckCircle2, FolderOpen, KeyRound, Loader2, Lock, LockOpen, Network, Settings2, ShieldAlert, ShieldCheck, SlidersHorizontal } from 'lucide-react'
import type { CertInfo, ConnectionConfig, ConnectionSecrets, ConnectionView, DbType, TlsMode } from '@shared/types'
import { DB_TYPES } from '@shared/types'
import { api, errorMessage } from '@/lib/api'
import { t } from '@/lib/i18n'
import { useStore } from '@/lib/store'
import { DbIcon, Modal } from '../ui'

const COLORS = ['', '#7c8cff', '#22c3a6', '#3ddc97', '#ffb547', '#ff6b6b', '#c792ea', '#79c0ff']

function isLocal(host: string) {
  const h = host.trim().toLowerCase()
  return !h || h === 'localhost' || h === '::1' || h.startsWith('127.') || h.startsWith('/')
}

function blank(type: DbType, folderId: string | null): ConnectionConfig {
  const def = DB_TYPES.find((d) => d.type === type)!
  return {
    id: '',
    name: '',
    type,
    folderId,
    host: type === 'sqlite' ? '' : 'localhost',
    port: def.defaultPort,
    database: type === 'postgres' ? 'postgres' : type === 'cockroach' ? 'defaultdb' : '',
    user: type === 'postgres' ? 'postgres' : type === 'mssql' ? 'sa' : type === 'mysql' || type === 'mariadb' ? 'root' : '',
    savePassword: true,
    readOnly: false,
    production: false,
    tls: { mode: 'verify-full' },
    ssh: { enabled: false, host: '', port: 22, user: '', auth: 'key', keyFile: '~/.ssh/id_ed25519' },
    createdAt: 0
  }
}

type TabKey = 'general' | 'security' | 'ssh' | 'advanced'

export function ConnectionDialog({ connection, folderId }: { connection?: ConnectionView; folderId?: string | null }) {
  const { setDialog, loadTree, toast } = useStore.getState()
  const folders = useStore((s) => s.tree.folders)
  const [cfg, setCfg] = useState<ConnectionConfig>(() => {
    if (connection) {
      // eslint-disable-next-line @typescript-eslint/no-unused-vars
      const { hasPassword, hasSshPassword, hasSshPassphrase, ...c } = connection
      return c
    }
    const c = blank('postgres', folderId ?? null)
    // Local development databases usually run without TLS.
    c.tls.mode = 'disabled'
    return c
  })
  const [secrets, setSecrets] = useState<ConnectionSecrets>({})
  const [tab, setTab] = useState<TabKey>('general')
  const [busy, setBusy] = useState<'test' | 'save' | 'cert' | null>(null)
  const [test, setTest] = useState<{ ok: boolean; message: string } | null>(null)
  const [cert, setCert] = useState<CertInfo | null>(null)

  const isNew = !connection
  const def = DB_TYPES.find((d) => d.type === cfg.type)!
  const isFile = !!def.file
  const set = (patch: Partial<ConnectionConfig>) => {
    setCfg((c) => ({ ...c, ...patch }))
    setTest(null)
  }
  const setTls = (patch: Partial<ConnectionConfig['tls']>) => set({ tls: { ...cfg.tls, ...patch } })
  const setSsh = (patch: Partial<ConnectionConfig['ssh']>) => set({ ssh: { ...cfg.ssh, ...patch } })

  const pinSupported = cfg.type === 'postgres' || cfg.type === 'cockroach'
  const insecureRemote = !isFile && cfg.tls.mode === 'disabled' && !cfg.ssh.enabled && !isLocal(cfg.host)
  const tlsModes: { mode: TlsMode; label: string; desc: string; icon: React.ReactNode }[] = useMemo(
    () => [
      { mode: 'verify-full', label: t('Verify full (recommended)'), desc: t('Encrypted. The certificate chain and the host name are verified - protects against man-in-the-middle attacks.'), icon: <ShieldCheck size={16} color="var(--ok)" /> },
      { mode: 'verify-ca', label: t('Verify CA'), desc: t('Encrypted. The certificate must be issued by the trusted CA; the host name is not checked (e.g. access via IP).'), icon: <ShieldCheck size={16} color="var(--warn)" /> },
      ...(pinSupported ? [{ mode: 'pinned' as TlsMode, label: t('Pinned certificate'), desc: t('Encrypted. Trusts exactly one certificate (SHA-256 fingerprint) - ideal for self-signed server certificates.'), icon: <KeyRound size={16} color="var(--accent)" /> }] : []),
      { mode: 'disabled', label: t('Disabled'), desc: t('Unencrypted. Only suitable for local databases or inside an SSH tunnel.'), icon: <LockOpen size={16} color="var(--danger)" /> }
    ],
    [pinSupported]
  )

  const pick = async (field: 'filePath' | 'caFile' | 'certFile' | 'keyFile' | 'sshKey', save = false) => {
    const filters =
      field === 'filePath'
        ? [{ name: 'SQLite', extensions: ['db', 'sqlite', 'sqlite3', 'db3'] }, { name: t('All files'), extensions: ['*'] }]
        : [{ name: t('All files'), extensions: ['*'] }]
    const p = save ? await api.pickSaveFile('database.db', filters) : await api.pickOpenFile(filters)
    if (!p) return
    if (field === 'filePath') set({ filePath: p, name: cfg.name || p.split(/[\\/]/).pop()?.replace(/\.\w+$/, '') || '' })
    else if (field === 'sshKey') setSsh({ keyFile: p })
    else setTls({ [field]: p })
  }

  const secretsArg = (): ConnectionSecrets | undefined => (Object.keys(secrets).length ? secrets : undefined)

  const runTest = async () => {
    setBusy('test')
    setTest(null)
    try {
      const r = await api.testConnection(cfg, secretsArg())
      if (r.hostKeyFingerprint) setSsh({ hostKeyFingerprint: r.hostKeyFingerprint })
      setTest({ ok: true, message: r.serverVersion.split('\n')[0] || t('Connected') })
    } catch (e) {
      setTest({ ok: false, message: errorMessage(e) })
    } finally {
      setBusy(null)
    }
  }

  const fetchCert = async () => {
    setBusy('cert')
    try {
      setCert(await api.fetchServerCert(cfg, secretsArg()))
    } catch (e) {
      toast(errorMessage(e), 'error')
    } finally {
      setBusy(null)
    }
  }

  const save = async () => {
    setBusy('save')
    try {
      const name = cfg.name.trim() || (isFile ? (cfg.filePath ?? '').split(/[\\/]/).pop() ?? 'SQLite' : `${cfg.host}${cfg.database ? '/' + cfg.database : ''}`)
      await api.saveConnection({ ...cfg, name }, secretsArg())
      await loadTree()
      setDialog(null)
      toast(isNew ? t('Connection created') : t('Connection saved'), 'success')
    } catch (e) {
      toast(errorMessage(e), 'error')
    } finally {
      setBusy(null)
    }
  }

  const tabs: { key: TabKey; label: string; icon: React.ReactNode }[] = [
    { key: 'general', label: t('General'), icon: <Settings2 size={14} /> },
    ...(!isFile
      ? [
          { key: 'security' as TabKey, label: t('TLS / Security'), icon: cfg.tls.mode === 'disabled' ? <LockOpen size={14} /> : <Lock size={14} /> },
          { key: 'ssh' as TabKey, label: t('SSH tunnel'), icon: <Network size={14} /> }
        ]
      : []),
    { key: 'advanced', label: t('Advanced'), icon: <SlidersHorizontal size={14} /> }
  ]

  return (
    <Modal
      title={isNew ? t('New connection') : t('Edit connection')}
      onClose={() => setDialog(null)}
      size="wide"
      tabs={
        <div className="modal-tabs">
          {tabs.map((x) => (
            <button key={x.key} className={tab === x.key ? 'on' : ''} onClick={() => setTab(x.key)}>
              {x.icon} {x.label}
            </button>
          ))}
        </div>
      }
      footer={
        <>
          {test && (
            <span className="row small grow" style={{ color: test.ok ? 'var(--ok)' : 'var(--danger)', minWidth: 0 }}>
              {test.ok ? <CheckCircle2 size={15} /> : <AlertTriangle size={15} />}
              <span className="ellipsis" title={test.message}>
                {test.message}
              </span>
            </span>
          )}
          <button className="btn" onClick={runTest} disabled={!!busy}>
            {busy === 'test' && <Loader2 size={14} className="spin" />} {t('Test connection')}
          </button>
          <button className="btn" onClick={() => setDialog(null)}>
            {t('Cancel')}
          </button>
          <button className="btn primary" onClick={save} disabled={!!busy}>
            {t('Save')}
          </button>
        </>
      }
    >
      {tab === 'general' && (
        <>
          {isNew && (
            <div className="type-grid">
              {DB_TYPES.map((d) => (
                <div
                  key={d.type}
                  className={`type-card ${cfg.type === d.type ? 'on' : ''}`}
                  onClick={() => {
                    const b = blank(d.type, cfg.folderId)
                    b.tls.mode = d.file || isLocal(cfg.host) ? 'disabled' : 'verify-full'
                    setCfg({ ...b, name: cfg.name, color: cfg.color, host: d.file ? '' : cfg.host || 'localhost' })
                    setTest(null)
                  }}
                >
                  <DbIcon type={d.type} size={28} />
                  {d.label}
                </div>
              ))}
            </div>
          )}
          <div className="grid2">
            <div className="field">
              <label>{t('Name')}</label>
              <input className="input" autoFocus value={cfg.name} placeholder={t('e.g. Shop production')} onChange={(e) => set({ name: e.target.value })} />
            </div>
            <div className="field">
              <label>{t('Folder')}</label>
              <select className="select" value={cfg.folderId ?? ''} onChange={(e) => set({ folderId: e.target.value || null })}>
                <option value="">{t('— none —')}</option>
                {folders.map((f) => (
                  <option key={f.id} value={f.id}>
                    {f.name}
                  </option>
                ))}
              </select>
            </div>
          </div>
          {isFile ? (
            <div className="field">
              <label>{t('Database file')}</label>
              <div className="row">
                <input className="input mono" value={cfg.filePath ?? ''} placeholder="/path/to/database.db" onChange={(e) => set({ filePath: e.target.value })} />
                <button className="btn" onClick={() => pick('filePath')}>
                  <FolderOpen size={14} /> {t('Open…')}
                </button>
                <button className="btn" onClick={() => pick('filePath', true)}>
                  {t('New…')}
                </button>
              </div>
            </div>
          ) : (
            <>
              <div className="grid3">
                <div className="field">
                  <label>{t('Host')}</label>
                  <input className="input mono" value={cfg.host} onChange={(e) => set({ host: e.target.value.trim() })} placeholder="db.example.com" />
                </div>
                <div className="field">
                  <label>{t('Port')}</label>
                  <input className="input mono" type="number" value={cfg.port || ''} onChange={(e) => set({ port: Number(e.target.value) || 0 })} />
                </div>
                <div className="field">
                  <label>{cfg.type === 'mysql' || cfg.type === 'mariadb' ? t('Database (optional)') : t('Database')}</label>
                  <input className="input mono" value={cfg.database} onChange={(e) => set({ database: e.target.value })} />
                </div>
              </div>
              {cfg.type === 'oracle' && (
                <div className="field">
                  <label>{t('Service name')}</label>
                  <input className="input mono" value={cfg.serviceName ?? ''} placeholder="ORCLPDB1" onChange={(e) => set({ serviceName: e.target.value })} />
                  <span className="hint">{t('Oracle requires the Oracle Instant Client to be installed on this computer.')}</span>
                </div>
              )}
              {cfg.type === 'mssql' && (
                <div className="field">
                  <label>{t('Instance name (optional)')}</label>
                  <input className="input mono" value={cfg.instanceName ?? ''} placeholder="SQLEXPRESS" onChange={(e) => set({ instanceName: e.target.value })} />
                </div>
              )}
              <div className="grid2">
                <div className="field">
                  <label>{t('User')}</label>
                  <input className="input mono" value={cfg.user} onChange={(e) => set({ user: e.target.value })} autoComplete="off" />
                </div>
                <div className="field">
                  <label>{t('Password')}</label>
                  <input
                    className="input"
                    type="password"
                    autoComplete="new-password"
                    placeholder={connection?.hasPassword ? t('•••••• (unchanged)') : ''}
                    value={secrets.password ?? ''}
                    onChange={(e) => setSecrets({ ...secrets, password: e.target.value })}
                  />
                </div>
              </div>
              <label className="check">
                <input type="checkbox" checked={cfg.savePassword} onChange={(e) => set({ savePassword: e.target.checked })} />
                <span>
                  {t('Save password in the system keychain')}
                  <div className="hint">{t('Passwords are never stored in plain text. Without saving, you will be asked when connecting.')}</div>
                </span>
              </label>
              {insecureRemote && (
                <div className="callout danger">
                  <ShieldAlert size={16} color="var(--danger)" />
                  <span>
                    {t('TLS is disabled for a remote host. Credentials and data would be sent unencrypted.')}{' '}
                    <a
                      href="#"
                      onClick={(e) => {
                        e.preventDefault()
                        setTab('security')
                      }}
                    >
                      {t('Configure TLS')}
                    </a>
                  </span>
                </div>
              )}
            </>
          )}
          <div className="field">
            <label>{t('Color')}</label>
            <div className="row">
              {COLORS.map((c) => (
                <button
                  key={c || 'none'}
                  onClick={() => set({ color: c || undefined })}
                  title={c || t('none')}
                  style={{
                    width: 22,
                    height: 22,
                    borderRadius: 6,
                    cursor: 'pointer',
                    background: c || 'transparent',
                    border: (cfg.color ?? '') === c ? '2px solid var(--text)' : '1px solid var(--border-strong)'
                  }}
                />
              ))}
            </div>
          </div>
        </>
      )}

      {tab === 'security' && (
        <>
          <div className="col" style={{ gap: 6 }}>
            {tlsModes.map((m) => (
              <label key={m.mode} className="check" style={{ padding: '8px 10px', borderRadius: 8, border: `1px solid ${cfg.tls.mode === m.mode ? 'var(--accent)' : 'var(--border)'}` }}>
                <input type="radio" name="tls" checked={cfg.tls.mode === m.mode} onChange={() => setTls({ mode: m.mode })} />
                <span className="col" style={{ gap: 2 }}>
                  <span className="row" style={{ gap: 6 }}>
                    {m.icon}
                    <b>{m.label}</b>
                  </span>
                  <span className="hint">{m.desc}</span>
                </span>
              </label>
            ))}
          </div>
          {(cfg.tls.mode === 'verify-full' || cfg.tls.mode === 'verify-ca') && (
            <div className="field">
              <label>{cfg.type === 'oracle' ? t('Wallet directory (optional)') : t('CA certificate (optional, PEM)')}</label>
              <div className="row">
                <input className="input mono" value={cfg.tls.caFile ?? ''} placeholder={t('System trust store')} onChange={(e) => setTls({ caFile: e.target.value || undefined })} />
                <button className="btn" onClick={() => pick('caFile')}>
                  <FolderOpen size={14} />
                </button>
              </div>
              <span className="hint">{t('Use this for servers with certificates from a private CA. The public CA list is then not used.')}</span>
            </div>
          )}
          {cfg.tls.mode === 'pinned' && (
            <div className="col">
              {cfg.tls.pinnedFingerprint ? (
                <div className="col" style={{ gap: 6 }}>
                  <span className="label">{t('Trusted certificate (SHA-256)')}</span>
                  <div className="fingerprint">{cfg.tls.pinnedFingerprint}</div>
                </div>
              ) : (
                <div className="callout warn">
                  <AlertTriangle size={16} color="var(--warn)" />
                  <span>{t('No certificate trusted yet. Fetch the server certificate and compare the fingerprint with the one on the server (e.g. openssl x509 -fingerprint -sha256).')}</span>
                </div>
              )}
              <div className="row">
                <button className="btn" onClick={fetchCert} disabled={!!busy}>
                  {busy === 'cert' && <Loader2 size={14} className="spin" />} {t('Fetch certificate')}
                </button>
              </div>
              {cert && (
                <div className="col" style={{ gap: 8, padding: 12, border: '1px solid var(--border)', borderRadius: 8 }}>
                  <div className="kv">
                    <span>{t('Subject')}</span>
                    <span className="mono small">{cert.subject}</span>
                    <span>{t('Issuer')}</span>
                    <span className="mono small">{cert.issuer}</span>
                    <span>{t('Valid')}</span>
                    <span className="small">
                      {cert.validFrom} – {cert.validTo}
                    </span>
                  </div>
                  <div className="fingerprint">{cert.fingerprint}</div>
                  <div className="row">
                    <button
                      className="btn primary"
                      onClick={() => {
                        setTls({ pinnedFingerprint: cert.fingerprint, pinnedCert: cert.pem })
                        setCert(null)
                      }}
                    >
                      <ShieldCheck size={14} /> {t('Trust this certificate')}
                    </button>
                    <span className="hint">{t('Only trust it if the fingerprint matches the server.')}</span>
                  </div>
                </div>
              )}
            </div>
          )}
          {cfg.tls.mode !== 'disabled' && (
            <div className="grid2">
              <div className="field">
                <label>{t('Client certificate (optional)')}</label>
                <div className="row">
                  <input className="input mono" value={cfg.tls.certFile ?? ''} onChange={(e) => setTls({ certFile: e.target.value || undefined })} />
                  <button className="btn" onClick={() => pick('certFile')}>
                    <FolderOpen size={14} />
                  </button>
                </div>
              </div>
              <div className="field">
                <label>{t('Client key (optional)')}</label>
                <div className="row">
                  <input className="input mono" value={cfg.tls.keyFile ?? ''} onChange={(e) => setTls({ keyFile: e.target.value || undefined })} />
                  <button className="btn" onClick={() => pick('keyFile')}>
                    <FolderOpen size={14} />
                  </button>
                </div>
              </div>
            </div>
          )}
          {cfg.tls.mode === 'disabled' && !isLocal(cfg.host) && !cfg.ssh.enabled && (
            <label className="check callout danger">
              <input type="checkbox" checked={!!cfg.tls.allowInsecure} onChange={(e) => setTls({ allowInsecure: e.target.checked })} />
              <span>{t('I understand that credentials and data are sent unencrypted to {host} and can be intercepted.', { host: cfg.host || '?' })}</span>
            </label>
          )}
        </>
      )}

      {tab === 'ssh' && (
        <>
          <label className="check">
            <input type="checkbox" checked={cfg.ssh.enabled} onChange={(e) => setSsh({ enabled: e.target.checked, host: cfg.ssh.host || '', user: cfg.ssh.user || '' })} />
            <span>
              <b>{t('Connect through an SSH tunnel')}</b>
              <div className="hint">{t('The database connection is forwarded through an encrypted SSH connection. The host key is verified against known_hosts or confirmed by you.')}</div>
            </span>
          </label>
          {cfg.ssh.enabled && (
            <>
              <div className="grid3">
                <div className="field">
                  <label>{t('SSH host')}</label>
                  <input className="input mono" value={cfg.ssh.host} onChange={(e) => setSsh({ host: e.target.value.trim() })} placeholder="bastion.example.com" />
                </div>
                <div className="field">
                  <label>{t('Port')}</label>
                  <input className="input mono" type="number" value={cfg.ssh.port} onChange={(e) => setSsh({ port: Number(e.target.value) || 22 })} />
                </div>
                <div className="field">
                  <label>{t('User')}</label>
                  <input className="input mono" value={cfg.ssh.user} onChange={(e) => setSsh({ user: e.target.value })} />
                </div>
              </div>
              <div className="field">
                <label>{t('Authentication')}</label>
                <div className="segmented" style={{ alignSelf: 'flex-start' }}>
                  {(['key', 'agent', 'password'] as const).map((a) => (
                    <button key={a} className={cfg.ssh.auth === a ? 'on' : ''} onClick={() => setSsh({ auth: a })}>
                      {a === 'key' ? t('Private key') : a === 'agent' ? t('SSH agent') : t('Password')}
                    </button>
                  ))}
                </div>
              </div>
              {cfg.ssh.auth === 'key' && (
                <div className="grid2">
                  <div className="field">
                    <label>{t('Private key file')}</label>
                    <div className="row">
                      <input className="input mono" value={cfg.ssh.keyFile ?? ''} onChange={(e) => setSsh({ keyFile: e.target.value })} />
                      <button className="btn" onClick={() => pick('sshKey')}>
                        <FolderOpen size={14} />
                      </button>
                    </div>
                  </div>
                  <div className="field">
                    <label>{t('Passphrase')}</label>
                    <input
                      className="input"
                      type="password"
                      placeholder={connection?.hasSshPassphrase ? t('•••••• (unchanged)') : t('none')}
                      value={secrets.sshPassphrase ?? ''}
                      onChange={(e) => setSecrets({ ...secrets, sshPassphrase: e.target.value })}
                    />
                  </div>
                </div>
              )}
              {cfg.ssh.auth === 'password' && (
                <div className="field">
                  <label>{t('SSH password')}</label>
                  <input
                    className="input"
                    type="password"
                    placeholder={connection?.hasSshPassword ? t('•••••• (unchanged)') : ''}
                    value={secrets.sshPassword ?? ''}
                    onChange={(e) => setSecrets({ ...secrets, sshPassword: e.target.value })}
                  />
                </div>
              )}
              <div className="field">
                <label>{t('Trusted host key')}</label>
                {cfg.ssh.hostKeyFingerprint ? (
                  <div className="row">
                    <div className="fingerprint grow">{cfg.ssh.hostKeyFingerprint}</div>
                    <button className="btn small" onClick={() => setSsh({ hostKeyFingerprint: undefined })}>
                      {t('Forget')}
                    </button>
                  </div>
                ) : (
                  <span className="hint">{t('Not yet known - it will be checked against ~/.ssh/known_hosts or shown for confirmation on the first connection.')}</span>
                )}
              </div>
              <span className="hint">{t('Host and port on the General tab are resolved from the SSH server (e.g. localhost:5432).')}</span>
            </>
          )}
        </>
      )}

      {tab === 'advanced' && (
        <>
          <label className="check">
            <input type="checkbox" checked={cfg.readOnly} onChange={(e) => set({ readOnly: e.target.checked })} />
            <span>
              <b>{t('Read-only')}</b>
              <div className="hint">{t('Data-modifying statements are blocked in SQLighter and, where supported, by the database session (READ ONLY).')}</div>
            </span>
          </label>
          <label className="check">
            <input type="checkbox" checked={cfg.production} onChange={(e) => set({ production: e.target.checked })} />
            <span>
              <b>{t('Production database')}</b>
              <div className="hint">{t('Every data-modifying statement and every table edit must be confirmed. Marked in red in the tree.')}</div>
            </span>
          </label>
        </>
      )}
    </Modal>
  )
}
