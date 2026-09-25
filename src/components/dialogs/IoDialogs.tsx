// Export, import, DB-to-DB transfer, backup and restore dialogs.
import { useEffect, useMemo, useState } from 'react'
import { ArrowRightLeft, DatabaseBackup, Download, FileUp, FolderOpen, History, Upload } from 'lucide-react'
import type { BackupOptions, DbObject, Dialect, ExportFormat, ExportSource, ImportFormat, ImportPreview, NativeTools } from '@shared/types'
import { dialectOf } from '@shared/types'
import { api, errorMessage } from '@/lib/api'
import { t } from '@/lib/i18n'
import { useStore } from '@/lib/store'
import { ResultGrid } from '../ResultGrid'
import { Modal } from '../ui'

const EXPORT_FORMATS: { f: ExportFormat; label: string; ext: string }[] = [
  { f: 'csv', label: 'CSV', ext: 'csv' },
  { f: 'tsv', label: 'TSV', ext: 'tsv' },
  { f: 'xlsx', label: 'Excel (XLSX)', ext: 'xlsx' },
  { f: 'json', label: 'JSON', ext: 'json' },
  { f: 'xml', label: 'XML', ext: 'xml' },
  { f: 'sql', label: 'SQL INSERT', ext: 'sql' },
  { f: 'markdown', label: 'Markdown', ext: 'md' },
  { f: 'html', label: 'HTML', ext: 'html' }
]

function started(taskLabel: string) {
  useStore.getState().toast(t('{task} started - progress is shown at the bottom right', { task: taskLabel }), 'info')
}

export function ExportDialog({ source, defaultName, dialect }: { source: ExportSource; defaultName: string; dialect?: Dialect }) {
  const { setDialog, toast } = useStore.getState()
  const [format, setFormat] = useState<ExportFormat>('csv')
  const [delimiter, setDelimiter] = useState(',')
  const [header, setHeader] = useState(true)
  const [nullText, setNullText] = useState('')
  const [pretty, setPretty] = useState(true)
  const [batch, setBatch] = useState(100)
  const [tableName, setTableName] = useState(defaultName)
  const fmt = EXPORT_FORMATS.find((x) => x.f === format)!

  const run = async () => {
    const path = await api.pickSaveFile(`${defaultName}.${fmt.ext}`, [{ name: fmt.label, extensions: [fmt.ext] }])
    if (!path) return
    try {
      const src: ExportSource = source.kind === 'rows' ? { ...source, tableName, dialect } : source.kind === 'query' ? { ...source, tableName } : source
      await api.exportData(src, { format, filePath: path, delimiter, includeHeader: header, nullText, prettyJson: pretty, batchSize: batch })
      setDialog(null)
      started(t('Export'))
    } catch (e) {
      toast(errorMessage(e), 'error')
    }
  }

  return (
    <Modal
      title={t('Export data')}
      icon={<Download size={18} />}
      onClose={() => setDialog(null)}
      footer={
        <>
          <button className="btn" onClick={() => setDialog(null)}>
            {t('Cancel')}
          </button>
          <button className="btn primary" onClick={run}>
            {t('Choose file and export…')}
          </button>
        </>
      }
    >
      <div className="hint">
        {source.kind === 'table'
          ? t('Exports the complete table {name}.', { name: `${source.table.schema}.${source.table.name}` })
          : source.kind === 'query'
            ? t('Runs the query again and exports all rows.')
            : t('Exports the {n} rows of the result.', { n: source.rows.length })}
      </div>
      <div className="type-grid">
        {EXPORT_FORMATS.map((x) => (
          <div key={x.f} className={`type-card ${format === x.f ? 'on' : ''}`} onClick={() => setFormat(x.f)}>
            <b>{x.ext.toUpperCase()}</b>
            <span className="muted small">{x.label}</span>
          </div>
        ))}
      </div>
      {format === 'csv' && (
        <div className="grid2">
          <div className="field">
            <label>{t('Delimiter')}</label>
            <select className="select" value={delimiter} onChange={(e) => setDelimiter(e.target.value)}>
              <option value=",">{t('Comma')} (,)</option>
              <option value=";">{t('Semicolon')} (;)</option>
              <option value="|">{t('Pipe')} (|)</option>
            </select>
          </div>
        </div>
      )}
      {(format === 'csv' || format === 'tsv' || format === 'xlsx') && (
        <label className="check">
          <input type="checkbox" checked={header} onChange={(e) => setHeader(e.target.checked)} /> {t('Include header row')}
        </label>
      )}
      {(format === 'csv' || format === 'tsv' || format === 'markdown' || format === 'html') && (
        <div className="field" style={{ maxWidth: 260 }}>
          <label>{t('Text for NULL values')}</label>
          <input className="input" value={nullText} placeholder={t('(empty)')} onChange={(e) => setNullText(e.target.value)} />
        </div>
      )}
      {format === 'json' && (
        <label className="check">
          <input type="checkbox" checked={pretty} onChange={(e) => setPretty(e.target.checked)} /> {t('Pretty print')}
        </label>
      )}
      {format === 'sql' && (
        <div className="grid2">
          {source.kind !== 'table' && (
            <div className="field">
              <label>{t('Target table name')}</label>
              <input className="input mono" value={tableName} onChange={(e) => setTableName(e.target.value)} />
            </div>
          )}
          <div className="field">
            <label>{t('Rows per INSERT')}</label>
            <input className="input" type="number" min={1} max={1000} value={batch} onChange={(e) => setBatch(Number(e.target.value) || 1)} />
          </div>
        </div>
      )}
    </Modal>
  )
}

const IMPORT_EXT: Record<string, ImportFormat> = { csv: 'csv', txt: 'csv', tsv: 'tsv', tab: 'tsv', json: 'json', xml: 'xml', xlsx: 'xlsx', xls: 'xlsx', xlsm: 'xlsx', ods: 'xlsx' }

export function ImportDialog({ connectionId, schema, table }: { connectionId: string; schema?: string; table?: string }) {
  const { setDialog, toast, loadObjects } = useStore.getState()
  const conn = useStore((s) => s.tree.connections.find((c) => c.id === connectionId))
  const summary = useStore((s) => s.conn[connectionId]?.summary)
  const [file, setFile] = useState<string | null>(null)
  const [format, setFormat] = useState<ImportFormat>('csv')
  const [delimiter, setDelimiter] = useState('')
  const [hasHeader, setHasHeader] = useState(true)
  const [sheet, setSheet] = useState<string | undefined>()
  const [preview, setPreview] = useState<ImportPreview | null>(null)
  const [targetSchema, setTargetSchema] = useState(schema ?? summary?.defaultSchema ?? '')
  const [objects, setObjects] = useState<DbObject[]>([])
  const [target, setTarget] = useState(table ?? '')
  const [createNew, setCreateNew] = useState(!table)
  const [truncate, setTruncate] = useState(false)
  const [mapping, setMapping] = useState<Record<string, string>>({})
  const [targetCols, setTargetCols] = useState<string[]>([])
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    useStore.getState().connect(connectionId)
  }, [connectionId])
  useEffect(() => {
    if (!targetSchema && summary?.defaultSchema) setTargetSchema(summary.defaultSchema)
  }, [summary, targetSchema])
  useEffect(() => {
    if (!targetSchema) return
    api.listObjects(connectionId, targetSchema).then((o) => setObjects(o.filter((x) => x.kind === 'table'))).catch(() => setObjects([]))
  }, [connectionId, targetSchema])
  useEffect(() => {
    if (createNew || !target || !targetSchema) {
      setTargetCols([])
      return
    }
    api
      .describeTable(connectionId, targetSchema, target)
      .then((i) => setTargetCols(i.columns.map((c) => c.name)))
      .catch(() => setTargetCols([]))
  }, [connectionId, targetSchema, target, createNew])

  const loadPreview = async (path: string, fmt: ImportFormat, opts: { delimiter?: string; hasHeader?: boolean; sheet?: string }) => {
    try {
      const p = await api.importPreview({ format: fmt, filePath: path, delimiter: opts.delimiter || undefined, hasHeader: opts.hasHeader, sheet: opts.sheet })
      setPreview(p)
      setError(null)
    } catch (e) {
      setPreview(null)
      setError(errorMessage(e))
    }
  }

  useEffect(() => {
    if (file) loadPreview(file, format, { delimiter, hasHeader, sheet })
  }, [file, format, delimiter, hasHeader, sheet])

  // Default mapping: same name (case-insensitive) or identical when creating a table
  useEffect(() => {
    if (!preview) return
    const m: Record<string, string> = {}
    for (const c of preview.columns) {
      if (createNew) m[c] = c.trim().replace(/\s+/g, '_')
      else m[c] = targetCols.find((x) => x.toLowerCase() === c.toLowerCase().trim().replace(/\s+/g, '_')) ?? targetCols.find((x) => x.toLowerCase() === c.toLowerCase()) ?? ''
    }
    setMapping(m)
  }, [preview, targetCols, createNew])

  const pick = async () => {
    const p = await api.pickOpenFile([
      { name: t('Data files'), extensions: ['csv', 'tsv', 'txt', 'json', 'xml', 'xlsx', 'xls', 'ods'] },
      { name: t('All files'), extensions: ['*'] }
    ])
    if (!p) return
    const ext = p.split('.').pop()?.toLowerCase() ?? ''
    setFormat(IMPORT_EXT[ext] ?? 'csv')
    setFile(p)
    setSheet(undefined)
    if (createNew && !target) setTarget((p.split(/[\\/]/).pop() ?? 'import').replace(/\.\w+$/, '').replace(/[^\w]+/g, '_').toLowerCase())
  }

  const run = async () => {
    if (!file || !preview || !target.trim()) return
    try {
      await api.importData({
        format,
        filePath: file,
        delimiter: delimiter || undefined,
        hasHeader,
        sheet,
        connectionId,
        table: { schema: targetSchema, name: target.trim() },
        createTable: createNew,
        truncate,
        mapping,
        batchSize: 500
      })
      setDialog(null)
      started(t('Import'))
      setTimeout(() => loadObjects(connectionId, targetSchema, true), 1500)
    } catch (e) {
      toast(errorMessage(e), 'error')
    }
  }

  const previewColumns = useMemo(() => (preview ? preview.columns.map((c) => ({ name: c })) : []), [preview])

  return (
    <Modal
      title={t('Import data into {name}', { name: conn?.name ?? '' })}
      icon={<Upload size={18} />}
      size="wide"
      onClose={() => setDialog(null)}
      footer={
        <>
          <span className="small muted grow">{preview ? t('{n} rows in file', { n: preview.totalRows }) : ''}</span>
          <button className="btn" onClick={() => setDialog(null)}>
            {t('Cancel')}
          </button>
          <button className="btn primary" disabled={!preview || !target.trim()} onClick={run}>
            {t('Import')}
          </button>
        </>
      }
    >
      <div className="row">
        <button className="btn" onClick={pick}>
          <FolderOpen size={14} /> {t('Choose file…')}
        </button>
        <span className="mono small ellipsis grow">{file ?? t('CSV, TSV, JSON, XML or Excel')}</span>
        <select className="select" style={{ width: 110 }} value={format} onChange={(e) => setFormat(e.target.value as ImportFormat)}>
          {(['csv', 'tsv', 'json', 'xml', 'xlsx'] as ImportFormat[]).map((f) => (
            <option key={f} value={f}>
              {f.toUpperCase()}
            </option>
          ))}
        </select>
      </div>
      {(format === 'csv' || format === 'xlsx' || format === 'tsv') && (
        <div className="row">
          {format === 'csv' && (
            <select className="select" style={{ width: 200 }} value={delimiter} onChange={(e) => setDelimiter(e.target.value)}>
              <option value="">{t('Delimiter: auto-detect')}</option>
              <option value=",">,</option>
              <option value=";">;</option>
              <option value="\t">Tab</option>
              <option value="|">|</option>
            </select>
          )}
          {format === 'xlsx' && preview?.sheets && (
            <select className="select" style={{ width: 200 }} value={sheet ?? preview.sheets[0]} onChange={(e) => setSheet(e.target.value)}>
              {preview.sheets.map((s) => (
                <option key={s}>{s}</option>
              ))}
            </select>
          )}
          <label className="check">
            <input type="checkbox" checked={hasHeader} onChange={(e) => setHasHeader(e.target.checked)} /> {t('First row contains column names')}
          </label>
        </div>
      )}
      {error && <div className="error-box">{error}</div>}
      {preview && (
        <div style={{ height: 180, display: 'flex', border: '1px solid var(--border)', borderRadius: 8, overflow: 'hidden' }}>
          <ResultGrid columns={previewColumns} rows={preview.rows} />
        </div>
      )}
      <div className="grid3">
        <div className="field">
          <label>{t('Target table')}</label>
          <div className="segmented" style={{ alignSelf: 'flex-start', marginBottom: 6 }}>
            <button className={createNew ? 'on' : ''} onClick={() => setCreateNew(true)}>
              {t('New table')}
            </button>
            <button className={!createNew ? 'on' : ''} onClick={() => setCreateNew(false)}>
              {t('Existing table')}
            </button>
          </div>
          {createNew ? (
            <input className="input mono" value={target} onChange={(e) => setTarget(e.target.value)} placeholder="new_table" />
          ) : (
            <select className="select" value={target} onChange={(e) => setTarget(e.target.value)}>
              <option value="">{t('— choose —')}</option>
              {objects.map((o) => (
                <option key={o.name}>{o.name}</option>
              ))}
            </select>
          )}
        </div>
        <div className="field">
          <label>{t('Schema')}</label>
          <select className="select" value={targetSchema} onChange={(e) => setTargetSchema(e.target.value)}>
            {(summary?.schemas ?? [targetSchema]).map((s) => (
              <option key={s}>{s}</option>
            ))}
          </select>
        </div>
        <div className="field">
          <label>&nbsp;</label>
          {!createNew && (
            <label className="check">
              <input type="checkbox" checked={truncate} onChange={(e) => setTruncate(e.target.checked)} /> {t('Delete existing rows first')}
            </label>
          )}
        </div>
      </div>
      {preview && (!createNew ? targetCols.length > 0 : true) && (
        <>
          <div className="section-title">{t('Column mapping')}</div>
          <div className="list-box" style={{ maxHeight: 200 }}>
            <table className="table-simple">
              <thead>
                <tr>
                  <th>{t('File column')}</th>
                  <th>{t('Target column')}</th>
                </tr>
              </thead>
              <tbody>
                {preview.columns.map((c) => (
                  <tr key={c}>
                    <td className="mono">{c}</td>
                    <td>
                      {createNew ? (
                        <input className="input sm mono" value={mapping[c] ?? ''} placeholder={t('(skip)')} onChange={(e) => setMapping({ ...mapping, [c]: e.target.value })} />
                      ) : (
                        <select className="select sm" value={mapping[c] ?? ''} onChange={(e) => setMapping({ ...mapping, [c]: e.target.value })}>
                          <option value="">{t('(skip)')}</option>
                          {targetCols.map((tc) => (
                            <option key={tc}>{tc}</option>
                          ))}
                        </select>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <span className="hint">{t('All rows are inserted in one transaction - on any error nothing is imported.')}</span>
        </>
      )}
    </Modal>
  )
}

export function TransferDialog({ sourceConnectionId, schema, tables }: { sourceConnectionId?: string; schema?: string; tables?: string[] }) {
  const { setDialog, toast } = useStore.getState()
  const connections = useStore((s) => s.tree.connections)
  const connState = useStore((s) => s.conn)
  const [src, setSrc] = useState(sourceConnectionId ?? connections[0]?.id ?? '')
  const [srcSchema, setSrcSchema] = useState(schema ?? '')
  const [dst, setDst] = useState(connections.find((c) => c.id !== src)?.id ?? '')
  const [dstSchema, setDstSchema] = useState('')
  const [objs, setObjs] = useState<DbObject[]>([])
  const [sel, setSel] = useState<Set<string>>(new Set(tables ?? []))
  const [create, setCreate] = useState(true)
  const [truncate, setTruncate] = useState(false)
  const [batch, setBatch] = useState(500)

  useEffect(() => {
    if (!src) return
    useStore
      .getState()
      .connect(src)
      .then(() => {
        const s = useStore.getState().conn[src]?.summary
        if (s && !srcSchema) setSrcSchema(s.defaultSchema)
      })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [src])
  useEffect(() => {
    if (!dst) return
    useStore
      .getState()
      .connect(dst)
      .then(() => {
        const s = useStore.getState().conn[dst]?.summary
        if (s) setDstSchema(s.defaultSchema)
      })
  }, [dst])
  useEffect(() => {
    if (!src || !srcSchema) return
    api.listObjects(src, srcSchema).then((o) => setObjs(o.filter((x) => x.kind === 'table'))).catch(() => setObjs([]))
  }, [src, srcSchema])

  const srcConn = connections.find((c) => c.id === src)
  const dstConn = connections.find((c) => c.id === dst)

  const run = async () => {
    try {
      await api.transferData({ sourceConnectionId: src, sourceSchema: srcSchema, tables: [...sel], targetConnectionId: dst, targetSchema: dstSchema, createTables: create, truncate, batchSize: batch })
      setDialog(null)
      started(t('Transfer'))
    } catch (e) {
      toast(errorMessage(e), 'error')
    }
  }

  return (
    <Modal
      title={t('Transfer data between databases')}
      icon={<ArrowRightLeft size={18} />}
      size="wide"
      onClose={() => setDialog(null)}
      footer={
        <>
          <span className="small muted grow">{t('{n} table(s) selected', { n: sel.size })}</span>
          <button className="btn" onClick={() => setDialog(null)}>
            {t('Cancel')}
          </button>
          <button className="btn primary" disabled={!sel.size || !dst || src === dst && srcSchema === dstSchema} onClick={run}>
            {t('Start transfer')}
          </button>
        </>
      }
    >
      <div className="grid2">
        <div className="col">
          <div className="section-title">{t('Source')}</div>
          <select className="select" value={src} onChange={(e) => (setSrc(e.target.value), setSrcSchema(''), setSel(new Set()))}>
            {connections.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
          <select className="select" value={srcSchema} onChange={(e) => (setSrcSchema(e.target.value), setSel(new Set()))}>
            {(connState[src]?.summary?.schemas ?? [srcSchema]).map((s) => (
              <option key={s}>{s}</option>
            ))}
          </select>
          <div className="row">
            <span className="label grow">{t('Tables')}</span>
            <button className="btn small ghost" onClick={() => setSel(new Set(objs.map((o) => o.name)))}>
              {t('All')}
            </button>
            <button className="btn small ghost" onClick={() => setSel(new Set())}>
              {t('None')}
            </button>
          </div>
          <div className="list-box">
            {objs.map((o) => (
              <label key={o.name} className="check">
                <input
                  type="checkbox"
                  checked={sel.has(o.name)}
                  onChange={(e) => {
                    const n = new Set(sel)
                    if (e.target.checked) n.add(o.name)
                    else n.delete(o.name)
                    setSel(n)
                  }}
                />
                <span className="mono">{o.name}</span>
              </label>
            ))}
            {!objs.length && <div className="hint" style={{ padding: 10 }}>{t('No tables')}</div>}
          </div>
        </div>
        <div className="col">
          <div className="section-title">{t('Target')}</div>
          <select className="select" value={dst} onChange={(e) => setDst(e.target.value)}>
            <option value="">{t('— choose —')}</option>
            {connections.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
          <select className="select" value={dstSchema} onChange={(e) => setDstSchema(e.target.value)}>
            {(connState[dst]?.summary?.schemas ?? [dstSchema]).map((s) => (
              <option key={s}>{s}</option>
            ))}
          </select>
          <label className="check">
            <input type="checkbox" checked={create} onChange={(e) => setCreate(e.target.checked)} />
            <span>
              {t('Create missing tables')}
              <div className="hint">
                {srcConn && dstConn && dialectOf(srcConn.type) !== dialectOf(dstConn.type)
                  ? t('Column types are converted from {a} to {b}.', { a: srcConn.type, b: dstConn.type })
                  : t('The structure is copied 1:1.')}
              </div>
            </span>
          </label>
          <label className="check">
            <input type="checkbox" checked={truncate} onChange={(e) => setTruncate(e.target.checked)} /> {t('Delete existing rows in target tables first')}
          </label>
          <div className="field" style={{ maxWidth: 200 }}>
            <label>{t('Batch size')}</label>
            <input className="input" type="number" min={1} max={10000} value={batch} onChange={(e) => setBatch(Number(e.target.value) || 500)} />
          </div>
          <span className="hint">{t('Each table is copied in its own transaction. Foreign keys are not created by the transfer.')}</span>
        </div>
      </div>
    </Modal>
  )
}

export function BackupDialog({ connectionId, schema }: { connectionId: string; schema?: string }) {
  const { setDialog, toast } = useStore.getState()
  const conn = useStore((s) => s.tree.connections.find((c) => c.id === connectionId))
  const summary = useStore((s) => s.conn[connectionId]?.summary)
  const [tools, setTools] = useState<NativeTools | null>(null)
  const [method, setMethod] = useState<BackupOptions['method']>('sql')
  const [sch, setSch] = useState(schema ?? '')
  const [ddl, setDdl] = useState(true)
  const [data, setData] = useState(true)
  const [drop, setDrop] = useState(false)
  const [gzip, setGzip] = useState(false)

  useEffect(() => {
    api.nativeTools().then(setTools)
    useStore.getState().connect(connectionId)
  }, [connectionId])
  useEffect(() => {
    if (!sch && summary?.defaultSchema) setSch(summary.defaultSchema)
  }, [summary, sch])

  if (!conn) return null
  const nativeAvailable =
    conn.type === 'sqlite' ||
    ((conn.type === 'postgres' || conn.type === 'cockroach') && !!tools?.pgDump) ||
    ((conn.type === 'mysql' || conn.type === 'mariadb') && !!(tools?.mysqldump || tools?.mariadbDump))
  const nativeName = conn.type === 'sqlite' ? t('SQLite online backup (copy of the database file)') : conn.type === 'postgres' || conn.type === 'cockroach' ? 'pg_dump' : 'mysqldump / mariadb-dump'

  const run = async () => {
    const ext = method === 'native' ? (conn.type === 'sqlite' ? 'db' : conn.type === 'postgres' || conn.type === 'cockroach' ? 'dump' : 'sql') : gzip ? 'sql.gz' : 'sql'
    const stamp = new Date().toISOString().slice(0, 16).replace(/[:T]/g, '-')
    const path = await api.pickSaveFile(`${conn.name.replace(/[^\w-]+/g, '_')}-${stamp}.${ext}`, [{ name: t('Backup'), extensions: [ext.split('.').pop()!] }])
    if (!path) return
    try {
      await api.backup({ connectionId, filePath: path, method, schema: sch || undefined, includeData: data, includeDdl: ddl, dropStatements: drop })
      setDialog(null)
      started(t('Backup'))
    } catch (e) {
      toast(errorMessage(e), 'error')
    }
  }

  return (
    <Modal
      title={t('Backup {name}', { name: conn.name })}
      icon={<DatabaseBackup size={18} />}
      onClose={() => setDialog(null)}
      footer={
        <>
          <button className="btn" onClick={() => setDialog(null)}>
            {t('Cancel')}
          </button>
          <button className="btn primary" onClick={run} disabled={method === 'native' && !nativeAvailable}>
            {t('Choose file and start…')}
          </button>
        </>
      }
    >
      <div className="col" style={{ gap: 6 }}>
        <label className="check" style={{ padding: '8px 10px', borderRadius: 8, border: `1px solid ${method === 'sql' ? 'var(--accent)' : 'var(--border)'}` }}>
          <input type="radio" checked={method === 'sql'} onChange={() => setMethod('sql')} />
          <span>
            <b>{t('Portable SQL dump')}</b>
            <div className="hint">{t('Written by SQLighter for every database: CREATE TABLE, indexes, foreign keys, views and INSERT statements. Restorable with "Restore / run SQL file".')}</div>
          </span>
        </label>
        <label className="check" style={{ padding: '8px 10px', borderRadius: 8, border: `1px solid ${method === 'native' ? 'var(--accent)' : 'var(--border)'}`, opacity: nativeAvailable ? 1 : 0.6 }}>
          <input type="radio" checked={method === 'native'} disabled={!nativeAvailable} onChange={() => setMethod('native')} />
          <span>
            <b>{t('Native tool')}: {nativeName}</b>
            <div className="hint">
              {nativeAvailable ? t('Uses the official tool of the database - most complete (functions, triggers, sequences, permissions).') : t('Not available: the tool was not found in PATH or is not supported for this database.')}
            </div>
          </span>
        </label>
      </div>
      {conn.type !== 'sqlite' && (
        <div className="field" style={{ maxWidth: 300 }}>
          <label>{conn.type === 'mysql' || conn.type === 'mariadb' ? t('Database') : t('Schema')}</label>
          <select className="select" value={sch} onChange={(e) => setSch(e.target.value)}>
            {(summary?.schemas ?? [sch]).map((s) => (
              <option key={s}>{s}</option>
            ))}
          </select>
        </div>
      )}
      {!(method === 'native' && conn.type === 'sqlite') && (
        <div className="col" style={{ gap: 6 }}>
          <label className="check">
            <input type="checkbox" checked={ddl} onChange={(e) => setDdl(e.target.checked)} /> {t('Structure (CREATE statements)')}
          </label>
          <label className="check">
            <input type="checkbox" checked={data} onChange={(e) => setData(e.target.checked)} /> {t('Data')}
          </label>
          <label className="check">
            <input type="checkbox" checked={drop} onChange={(e) => setDrop(e.target.checked)} /> {t('Add DROP statements (overwrite on restore)')}
          </label>
          {method === 'sql' && (
            <label className="check">
              <input type="checkbox" checked={gzip} onChange={(e) => setGzip(e.target.checked)} /> {t('Compress (gzip)')}
            </label>
          )}
        </div>
      )}
    </Modal>
  )
}

export function RestoreDialog({ connectionId }: { connectionId: string }) {
  const { setDialog, toast } = useStore.getState()
  const conn = useStore((s) => s.tree.connections.find((c) => c.id === connectionId))
  const [file, setFile] = useState<string | null>(null)
  const [stop, setStop] = useState(true)

  const run = () => {
    if (!file || !conn) return
    setDialog({
      type: 'confirm',
      danger: true,
      title: t('Run {file} on {name}?', { file: file.split(/[\\/]/).pop() ?? file, name: conn.name }),
      message: t('All statements in the file are executed. Existing data may be overwritten.'),
      confirmLabel: t('Restore'),
      onConfirm: async () => {
        try {
          await api.restore({ connectionId, filePath: file, stopOnError: stop })
          started(t('Restore'))
        } catch (e) {
          toast(errorMessage(e), 'error')
        }
      }
    })
  }

  return (
    <Modal
      title={t('Restore / run SQL file')}
      icon={<History size={18} />}
      size="narrow"
      onClose={() => setDialog(null)}
      footer={
        <>
          <button className="btn" onClick={() => setDialog(null)}>
            {t('Cancel')}
          </button>
          <button className="btn primary" disabled={!file} onClick={run}>
            {t('Restore…')}
          </button>
        </>
      }
    >
      <div className="hint">{t('Runs a SQL dump (.sql, .sql.gz) statement by statement, or a PostgreSQL custom archive (.dump) with pg_restore.')}</div>
      <div className="row">
        <button
          className="btn"
          onClick={async () => {
            const p = await api.pickOpenFile([
              { name: t('Backups'), extensions: ['sql', 'gz', 'dump', 'backup'] },
              { name: t('All files'), extensions: ['*'] }
            ])
            if (p) setFile(p)
          }}
        >
          <FileUp size={14} /> {t('Choose file…')}
        </button>
        <span className="mono small ellipsis">{file}</span>
      </div>
      <label className="check">
        <input type="checkbox" checked={stop} onChange={(e) => setStop(e.target.checked)} /> {t('Stop on the first error')}
      </label>
    </Modal>
  )
}
