// SQL editor tab: editor + results.
import { useEffect, useMemo, useRef, useState } from 'react'
import type { EditorView } from '@codemirror/view'
import {
  AlignLeft,
  CheckCheck,
  CircleAlert,
  CircleCheck,
  Download,
  FileCode2,
  Gauge,
  History,
  Play,
  PlaySquare,
  Save,
  Sparkles,
  Square,
  Undo2,
  Wand2
} from 'lucide-react'
import type { Dialect, QueryResult } from '@shared/types'
import { dialectOf } from '@shared/types'
import { statementAt } from '@shared/sql'
import { api, errorMessage } from '@/lib/api'
import { formatCount, formatDuration } from '@/lib/format'
import { t } from '@/lib/i18n'
import { editors, useStore, type SqlTab as SqlTabT } from '@/lib/store'
import { Editor } from './Editor'
import { ResultGrid } from './ResultGrid'
import { HSplitter, usePersistentState } from './ui'
import { askAi } from './ChatPanel'

const FORMATTER_LANG: Record<Dialect, 'postgresql' | 'mysql' | 'sqlite' | 'transactsql' | 'plsql'> = {
  postgres: 'postgresql',
  mysql: 'mysql',
  sqlite: 'sqlite',
  mssql: 'transactsql',
  oracle: 'plsql'
}

const completionCache = new Map<string, Promise<Record<string, string[]>>>()

export function useCompletion(connectionId: string | null, schema: string | undefined, connected: boolean) {
  const [schemaMap, setSchemaMap] = useState<Record<string, string[]> | undefined>()
  useEffect(() => {
    if (!connectionId || !connected || !schema) {
      setSchemaMap(undefined)
      return
    }
    const key = `${connectionId}|${schema}`
    let p = completionCache.get(key)
    if (!p) {
      p = api.completionSchema(connectionId, schema).catch(() => ({}))
      completionCache.set(key, p)
      setTimeout(() => completionCache.delete(key), 60_000)
    }
    let alive = true
    p.then((m) => alive && setSchemaMap(m))
    return () => {
      alive = false
    }
  }, [connectionId, schema, connected])
  return schemaMap
}

export function SqlTab({ tab, visible }: { tab: SqlTabT; visible: boolean }) {
  const tree = useStore((s) => s.tree)
  const connState = useStore((s) => (tab.connectionId ? s.conn[tab.connectionId] : undefined))
  const settings = useStore((s) => s.settings)
  const { updateTab, connect, setDialog, toast, setTxState } = useStore.getState()
  const conn = tree.connections.find((c) => c.id === tab.connectionId)
  const dialect: Dialect = conn ? dialectOf(conn.type) : 'postgres'
  const connected = connState?.status === 'connected'
  const schema = tab.schema || connState?.summary?.defaultSchema
  const completion = useCompletion(tab.connectionId, schema, connected)
  const viewRef = useRef<EditorView | null>(null)
  const [results, setResults] = useState<QueryResult[]>([])
  const [activeResult, setActiveResult] = useState(0)
  const [running, setRunning] = useState(false)
  const [resultsH, setResultsH] = usePersistentState('sqlighter.resultsH', 320)
  const [filter, setFilter] = useState('')
  const lastSql = useRef('')

  const runSql = async (sql: string, opts: { confirmed?: boolean; maxRows?: number } = {}) => {
    if (!sql.trim()) return
    if (!tab.connectionId) {
      toast(t('Choose a connection first'), 'error')
      return
    }
    if (!connected && !(await connect(tab.connectionId))) return
    lastSql.current = sql
    setRunning(true)
    try {
      const res = await api.execute(tab.connectionId, sql, { confirmed: opts.confirmed, maxRows: opts.maxRows })
      if (res.needsConfirmation) {
        const nc = res.needsConfirmation
        const isProd = nc.reason === 'production'
        setDialog({
          type: 'confirm',
          danger: true,
          title: isProd ? t('Modify production database?') : t('Run destructive statement?'),
          message: isProd
            ? t('"{name}" is marked as production. The following statements will modify data:', { name: conn?.name ?? '' })
            : t('The following statements can delete data ({reason}):', { reason: nc.reason }),
          details: nc.statements.join(';\n\n'),
          confirmLabel: t('Execute'),
          onConfirm: () => runSql(sql, { ...opts, confirmed: true })
        })
        return
      }
      setResults(res.results)
      setTxState(tab.connectionId, res.inTransaction)
      const firstErr = res.results.findIndex((r) => r.error)
      const lastSet = res.results.map((r, i) => (r.isResultSet ? i : -1)).filter((i) => i >= 0)
      setActiveResult(firstErr >= 0 ? firstErr : lastSet.length ? lastSet[0] : Math.max(0, res.results.length - 1))
      setFilter('')
    } catch (e) {
      setResults([{ statement: sql, columns: [], rows: [], truncated: false, durationMs: 0, error: errorMessage(e), isResultSet: false }])
      setActiveResult(0)
    } finally {
      setRunning(false)
    }
  }

  const currentStatement = (v: EditorView): string => {
    const sel = v.state.sliceDoc(v.state.selection.main.from, v.state.selection.main.to)
    if (sel.trim()) return sel
    const doc = v.state.doc.toString()
    return statementAt(doc, v.state.selection.main.head, dialect)?.text ?? ''
  }

  const runCurrent = () => {
    const v = viewRef.current
    if (v) runSql(currentStatement(v))
  }
  const runScript = () => {
    const v = viewRef.current
    if (!v) return
    const sel = v.state.sliceDoc(v.state.selection.main.from, v.state.selection.main.to)
    runSql(sel.trim() ? sel : v.state.doc.toString())
  }
  const explain = () => {
    const v = viewRef.current
    if (!v) return
    const stmt = currentStatement(v)
    if (!stmt.trim()) return
    const prefix = dialect === 'sqlite' ? 'EXPLAIN QUERY PLAN ' : dialect === 'postgres' || dialect === 'mysql' ? 'EXPLAIN ' : null
    if (!prefix) {
      toast(t('Explain is available for PostgreSQL, MySQL/MariaDB and SQLite'), 'info')
      return
    }
    runSql(prefix + stmt)
  }
  const format = async () => {
    const v = viewRef.current
    if (!v) return
    const { from, to } = v.state.selection.main
    const hasSel = to > from
    const src = hasSel ? v.state.sliceDoc(from, to) : v.state.doc.toString()
    try {
      const { format: formatSql } = await import('sql-formatter')
      const out = formatSql(src, { language: FORMATTER_LANG[dialect], keywordCase: 'upper', tabWidth: 2 })
      v.dispatch({ changes: hasSel ? { from, to, insert: out } : { from: 0, to: v.state.doc.length, insert: out } })
    } catch (e) {
      toast(t('Could not format: {error}', { error: errorMessage(e) }), 'error')
    }
  }
  const save = async () => {
    let path = tab.filePath
    if (!path) {
      path = (await api.pickSaveFile(`${tab.title.replace(/[^\w.-]+/g, '_')}.sql`, [{ name: 'SQL', extensions: ['sql'] }])) ?? undefined
      if (!path) return
    }
    try {
      await api.writeTextFile(path, viewRef.current?.state.doc.toString() ?? tab.sql)
      updateTab(tab.id, { filePath: path, dirty: false, title: path.split(/[\\/]/).pop() ?? tab.title })
      toast(t('Saved {path}', { path }), 'success')
    } catch (e) {
      toast(errorMessage(e), 'error')
    }
  }

  useEffect(() => {
    editors.set(tab.id, {
      insert: (text) => {
        const v = viewRef.current
        if (!v) return
        const { from, to } = v.state.selection.main
        const pre = from > 0 && v.state.sliceDoc(from - 1, from) !== '\n' ? '\n' : ''
        v.dispatch({ changes: { from, to, insert: pre + text }, selection: { anchor: from + pre.length + text.length } })
        v.focus()
      },
      replace: (text) => {
        const v = viewRef.current
        if (!v) return
        v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: text } })
        v.focus()
      },
      getSql: () => viewRef.current?.state.doc.toString() ?? tab.sql,
      getSelection: () => {
        const v = viewRef.current
        return v ? v.state.sliceDoc(v.state.selection.main.from, v.state.selection.main.to) : ''
      },
      run: (sql) => (sql ? runSql(sql) : runCurrent()),
      focus: () => viewRef.current?.focus()
    })
    return () => {
      editors.delete(tab.id)
    }
  })

  const txManual = connState?.autoCommit === false
  const current = results[activeResult]
  const filteredRows = useMemo(() => {
    if (!current || !filter.trim()) return current?.rows ?? []
    const f = filter.toLowerCase()
    return current.rows.filter((r) => r.some((v) => v !== null && String(v).toLowerCase().includes(f)))
  }, [current, filter])

  const schemas = connState?.summary?.schemas ?? []

  return (
    <div className="tabpane" style={{ display: visible ? 'flex' : 'none' }}>
      <div className="toolbar">
        <select
          className="select sm"
          value={tab.connectionId ?? ''}
          onChange={(e) => updateTab(tab.id, { connectionId: e.target.value || null, schema: undefined })}
          title={t('Connection')}
        >
          <option value="">{t('— no connection —')}</option>
          {tree.connections.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </select>
        {connected && schemas.length > 0 && (
          <select className="select sm" value={schema ?? ''} onChange={(e) => updateTab(tab.id, { schema: e.target.value })} title={t('Schema')}>
            {schemas.map((s) => (
              <option key={s} value={s}>
                {s}
              </option>
            ))}
          </select>
        )}
        <div className="toolbar-sep" />
        {running ? (
          <button className="btn small danger" onClick={() => tab.connectionId && api.cancelQuery(tab.connectionId)}>
            <Square size={13} /> {t('Stop')}
          </button>
        ) : (
          <button className="btn small primary" onClick={runCurrent} title={t('Run statement at cursor (Ctrl+Enter)')}>
            <Play size={13} /> {t('Run')}
          </button>
        )}
        <button className="icon-btn" onClick={runScript} disabled={running} title={t('Run script (Ctrl+Shift+Enter)')}>
          <PlaySquare size={16} />
        </button>
        <button className="icon-btn" onClick={explain} disabled={running} title={t('Explain (Ctrl+Shift+E)')}>
          <Gauge size={16} />
        </button>
        <button className="icon-btn" onClick={format} title={t('Format SQL (Ctrl+Shift+F)')}>
          <AlignLeft size={16} />
        </button>
        <button className="icon-btn" onClick={save} title={t('Save as file (Ctrl+S)')}>
          <Save size={16} />
        </button>
        <button className="icon-btn" onClick={() => setDialog({ type: 'history', connectionId: tab.connectionId ?? undefined })} title={t('Query history')}>
          <History size={16} />
        </button>
        <div className="toolbar-sep" />
        {tab.connectionId && connected && (
          <>
            <div className="segmented" title={t('Transaction mode')}>
              <button
                className={!txManual ? 'on' : ''}
                onClick={async () => {
                  await api.setAutoCommit(tab.connectionId!, true).catch((e) => toast(errorMessage(e), 'error'))
                  useStore.setState((s) => ({ conn: { ...s.conn, [tab.connectionId!]: { ...s.conn[tab.connectionId!], autoCommit: true, inTransaction: false } } }))
                }}
              >
                {t('Auto')}
              </button>
              <button
                className={txManual ? 'on' : ''}
                onClick={async () => {
                  await api.setAutoCommit(tab.connectionId!, false).catch((e) => toast(errorMessage(e), 'error'))
                  useStore.setState((s) => ({ conn: { ...s.conn, [tab.connectionId!]: { ...s.conn[tab.connectionId!], autoCommit: false } } }))
                }}
              >
                {t('Manual')}
              </button>
            </div>
            {(txManual || connState?.inTransaction) && (
              <>
                <button
                  className="btn small"
                  disabled={!connState?.inTransaction}
                  onClick={async () => {
                    await api.commit(tab.connectionId!).then(() => toast(t('Committed'), 'success')).catch((e) => toast(errorMessage(e), 'error'))
                    setTxState(tab.connectionId!, false)
                  }}
                >
                  <CheckCheck size={13} /> {t('Commit')}
                </button>
                <button
                  className="btn small"
                  disabled={!connState?.inTransaction}
                  onClick={async () => {
                    await api.rollback(tab.connectionId!).then(() => toast(t('Rolled back'), 'info')).catch((e) => toast(errorMessage(e), 'error'))
                    setTxState(tab.connectionId!, false)
                  }}
                >
                  <Undo2 size={13} /> {t('Rollback')}
                </button>
              </>
            )}
          </>
        )}
        <div className="grow" />
        <button
          className="btn small ghost"
          onClick={() => {
            const v = viewRef.current
            const sel = v ? v.state.sliceDoc(v.state.selection.main.from, v.state.selection.main.to) : ''
            askAi(sel.trim() ? t('Explain and improve this SQL:\n\n```sql\n{sql}\n```', { sql: sel.trim() }) : '')
          }}
          title={t('Ask the AI assistant (Ctrl+J)')}
        >
          <Sparkles size={14} /> {t('Ask AI')}
        </button>
      </div>
      <Editor
        value={tab.sql}
        onChange={(v) => updateTab(tab.id, { sql: v, dirty: !!tab.filePath })}
        dialect={dialect}
        schema={completion}
        defaultSchema={schema}
        fontSize={settings?.editorFontSize}
        placeholder={t('Write SQL here… Ctrl+Enter runs the statement at the cursor, Ctrl+Shift+Enter runs everything.')}
        onRun={runCurrent}
        onRunScript={runScript}
        onExplain={explain}
        onFormat={format}
        onSave={save}
        onReady={(v) => (viewRef.current = v)}
      />
      {(results.length > 0 || running) && (
        <>
          <HSplitter height={resultsH} onChange={setResultsH} min={100} />
          <div className="results" style={{ height: resultsH }}>
            <div className="results-tabs">
              {results.map((r, i) => (
                <div key={i} className={`rtab ${i === activeResult ? 'active' : ''}`} onClick={() => setActiveResult(i)} title={r.statement}>
                  {r.error ? <CircleAlert size={13} color="var(--danger)" /> : r.isResultSet ? <FileCode2 size={13} /> : <CircleCheck size={13} color="var(--ok)" />}
                  {r.isResultSet ? t('Result {n}', { n: i + 1 }) : r.error ? t('Error') : t('Statement {n}', { n: i + 1 })}
                </div>
              ))}
              {running && <span className="badge accent">{t('Running…')}</span>}
              <div className="grow" />
              {current?.isResultSet && (
                <>
                  <input className="input sm" style={{ width: 180 }} placeholder={t('Filter rows…')} value={filter} onChange={(e) => setFilter(e.target.value)} />
                  <span className="small muted">
                    {formatCount(current.rows.length)} {t('rows')}
                    {current.truncated && ` (${t('limited')})`} · {formatDuration(current.durationMs)}
                  </span>
                  {current.truncated && (
                    <button className="btn small" onClick={() => runSql(current.statement, { maxRows: 1_000_000 })} title={t('Fetch all rows')}>
                      {t('Fetch all')}
                    </button>
                  )}
                  <button
                    className="icon-btn"
                    title={t('Generate SQL from result')}
                    onClick={() => setDialog({ type: 'generate', connectionId: tab.connectionId ?? '', dialect, columns: current.columns, rows: current.rows })}
                  >
                    <Wand2 size={15} />
                  </button>
                  <button
                    className="icon-btn"
                    title={t('Export result')}
                    onClick={() =>
                      setDialog({
                        type: 'export',
                        dialect,
                        defaultName: 'result',
                        source: current.truncated && tab.connectionId ? { kind: 'query', connectionId: tab.connectionId, sql: current.statement } : { kind: 'rows', columns: current.columns, rows: current.rows, dialect }
                      })
                    }
                  >
                    <Download size={15} />
                  </button>
                </>
              )}
            </div>
            {current?.error ? (
              <div className="result-message">
                <div className="error-box">{current.error}</div>
                <div className="small muted mono" style={{ marginTop: 10, whiteSpace: 'pre-wrap' }}>
                  {current.statement}
                </div>
              </div>
            ) : current?.isResultSet ? (
              <ResultGrid
                columns={current.columns}
                rows={filteredRows}
                extraMenu={(sel) => [
                  {
                    label: t('Generate SQL for selected rows…'),
                    icon: <Wand2 size={14} />,
                    disabled: !sel.rows.length,
                    onClick: () =>
                      setDialog({
                        type: 'generate',
                        connectionId: tab.connectionId ?? '',
                        dialect,
                        columns: current.columns,
                        rows: sel.rows.map((r) => filteredRows[r])
                      })
                  }
                ]}
              />
            ) : current ? (
              <div className="result-message">
                <div className="row">
                  <CircleCheck size={16} color="var(--ok)" />
                  <b>{current.affectedRows != null ? t('{n} row(s) affected', { n: formatCount(current.affectedRows) }) : t('Statement executed')}</b>
                  <span className="muted">· {formatDuration(current.durationMs)}</span>
                </div>
                <div className="small muted mono" style={{ marginTop: 8, whiteSpace: 'pre-wrap' }}>
                  {current.statement}
                </div>
                {results.length > 1 && (
                  <div className="small muted" style={{ marginTop: 12 }}>
                    {t('{ok} of {n} statements succeeded', { ok: results.filter((r) => !r.error).length, n: results.length })}
                  </div>
                )}
              </div>
            ) : null}
          </div>
        </>
      )}
    </div>
  )
}
