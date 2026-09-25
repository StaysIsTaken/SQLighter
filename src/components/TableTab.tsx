// Table tab: editable data grid, structure and DDL.
import { useCallback, useEffect, useMemo, useState } from 'react'
import { ChevronLeft, ChevronRight, Copy, Download, FileCode2, KeyRound, Plus, RefreshCw, Save, Trash2, Undo2, Upload, Wand2 } from 'lucide-react'
import type { CellValue, Dialect, QueryResult, RowChange, TableInfo } from '@shared/types'
import { dialectOf } from '@shared/types'
import { buildChangeStatements, canEditTable } from '@shared/sqlgen'
import { api, errorMessage } from '@/lib/api'
import { copyText, formatCount, formatDuration } from '@/lib/format'
import { t } from '@/lib/i18n'
import { useStore, type TableTab as TableTabT } from '@/lib/store'
import { Editor } from './Editor'
import { ResultGrid } from './ResultGrid'

export function TableTab({ tab, visible }: { tab: TableTabT; visible: boolean }) {
  const conn = useStore((s) => s.tree.connections.find((c) => c.id === tab.connectionId))
  const { updateTab } = useStore.getState()
  const dialect: Dialect = conn ? dialectOf(conn.type) : 'postgres'
  const [info, setInfo] = useState<TableInfo | null>(null)
  const [infoError, setInfoError] = useState<string | null>(null)

  const loadInfo = useCallback(async () => {
    try {
      if (!(await useStore.getState().connect(tab.connectionId))) return
      setInfo(await api.describeTable(tab.connectionId, tab.schema, tab.name, tab.objectKind))
      setInfoError(null)
    } catch (e) {
      setInfoError(errorMessage(e))
    }
  }, [tab.connectionId, tab.schema, tab.name, tab.objectKind])

  useEffect(() => {
    loadInfo()
  }, [loadInfo])

  return (
    <div className="tabpane" style={{ display: visible ? 'flex' : 'none' }}>
      <div className="toolbar">
        <div className="segmented">
          {(['data', 'structure', 'ddl'] as const).map((v) => (
            <button key={v} className={tab.view === v ? 'on' : ''} onClick={() => updateTab(tab.id, { view: v })}>
              {v === 'data' ? t('Data') : v === 'structure' ? t('Structure') : 'DDL'}
            </button>
          ))}
        </div>
        <span className="small muted ellipsis">
          {conn?.name} · {tab.schema}.{tab.name}
        </span>
        {info?.rowEstimate != null && <span className="badge">~{formatCount(info.rowEstimate)} {t('rows')}</span>}
      </div>
      {infoError && (
        <div className="result-message">
          <div className="error-box">{infoError}</div>
        </div>
      )}
      {tab.view === 'data' && <DataView tab={tab} info={info} dialect={dialect} readOnly={!!conn?.readOnly} />}
      {tab.view === 'structure' && info && <StructureView info={info} />}
      {tab.view === 'ddl' && <DdlView tab={tab} dialect={dialect} />}
    </div>
  )
}

function DataView({ tab, info, dialect, readOnly }: { tab: TableTabT; info: TableInfo | null; dialect: Dialect; readOnly: boolean }) {
  const { setDialog, toast, openSqlTab } = useStore.getState()
  const [data, setData] = useState<QueryResult | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [where, setWhere] = useState('')
  const [appliedWhere, setAppliedWhere] = useState('')
  const [sort, setSort] = useState<{ col: number; desc: boolean } | null>(null)
  const [pageSize, setPageSize] = useState(200)
  const [offset, setOffset] = useState(0)
  const [edits, setEdits] = useState<Map<number, Map<number, CellValue>>>(new Map())
  const [newRows, setNewRows] = useState<CellValue[][]>([])
  const [deleted, setDeleted] = useState<Set<number>>(new Set())
  const [selRows, setSelRows] = useState<number[]>([])

  const editable = !!info && !readOnly && canEditTable(info)
  const pending = edits.size + newRows.length + deleted.size

  const load = useCallback(async () => {
    setLoading(true)
    try {
      const cols = data?.columns
      const r = await api.tableData({
        connectionId: tab.connectionId,
        table: { schema: tab.schema, name: tab.name },
        whereClause: appliedWhere || undefined,
        orderBy: sort && cols?.[sort.col] ? [{ column: cols[sort.col].name, desc: sort.desc }] : [],
        limit: pageSize,
        offset
      })
      setData(r)
      setError(null)
      setEdits(new Map())
      setNewRows([])
      setDeleted(new Set())
    } catch (e) {
      setError(errorMessage(e))
    } finally {
      setLoading(false)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab.connectionId, tab.schema, tab.name, appliedWhere, sort, pageSize, offset])

  useEffect(() => {
    load()
  }, [load])

  const rows = useMemo(() => (data ? [...data.rows, ...newRows] : []), [data, newRows])
  const baseCount = data?.rows.length ?? 0

  const onEditCell = (row: number, col: number, value: CellValue) => {
    if (row >= baseCount) {
      setNewRows((nr) => nr.map((r, i) => (i === row - baseCount ? r.map((v, ci) => (ci === col ? value : v)) : r)))
      return
    }
    setEdits((m) => {
      const next = new Map(m)
      const rowEdits = new Map(next.get(row) ?? [])
      const orig = data?.rows[row]?.[col] ?? null
      if (orig === value) rowEdits.delete(col)
      else rowEdits.set(col, value)
      if (rowEdits.size) next.set(row, rowEdits)
      else next.delete(row)
      return next
    })
  }

  const rowState = (r: number) => (r >= baseCount ? 'new' : deleted.has(r) ? 'deleted' : undefined)

  const buildChanges = (): RowChange[] => {
    if (!data || !info) return []
    const colNames = data.columns.map((c) => c.name)
    const obj = (r: CellValue[]) => Object.fromEntries(colNames.map((n, i) => [n, r[i]]))
    const changes: RowChange[] = []
    for (const r of deleted) changes.push({ kind: 'delete', original: obj(data.rows[r]) })
    for (const [r, m] of edits) {
      if (deleted.has(r)) continue
      const values: Record<string, CellValue> = {}
      for (const [c, v] of m) values[colNames[c]] = v
      changes.push({ kind: 'update', original: obj(data.rows[r]), values })
    }
    for (const nr of newRows) {
      const values: Record<string, CellValue> = {}
      nr.forEach((v, i) => {
        // Leave untouched (null) auto-increment / defaulted columns to the database.
        const ci = info.columns.find((c) => c.name === colNames[i])
        if (v === null && (ci?.autoIncrement || ci?.defaultValue != null)) return
        values[colNames[i]] = v
      })
      changes.push({ kind: 'insert', values })
    }
    return changes
  }

  const save = () => {
    if (!info) return
    const stmts = buildChangeStatements(info, buildChanges(), dialect)
    if (!stmts.length) return
    setDialog({
      type: 'confirm',
      title: t('Save {n} change(s)?', { n: pending }),
      message: t('These statements will be executed in one transaction:'),
      details: stmts.map((s) => s + ';').join('\n'),
      confirmLabel: t('Save'),
      onConfirm: async () => {
        try {
          const n = await api.applyChanges(tab.connectionId, stmts)
          toast(t('Saved · {n} row(s) affected', { n }), 'success')
          load()
        } catch (e) {
          toast(errorMessage(e), 'error')
        }
      }
    })
  }

  const addRow = () => {
    if (!data) return
    setNewRows((nr) => [...nr, data.columns.map(() => null)])
  }

  const deleteSelected = () => {
    if (!selRows.length) return
    setDeleted((d) => {
      const n = new Set(d)
      for (const r of selRows) {
        if (r >= baseCount) continue
        if (n.has(r)) n.delete(r)
        else n.add(r)
      }
      return n
    })
    const newSel = selRows.filter((r) => r >= baseCount).map((r) => r - baseCount)
    if (newSel.length) setNewRows((nr) => nr.filter((_, i) => !newSel.includes(i)))
  }

  const selectedRowObjects = (rs: number[]) => {
    if (!data) return []
    return rs.map((r) => rows[r])
  }

  return (
    <>
      <div className="toolbar">
        <span className="mono small muted">WHERE</span>
        <input
          className="input sm mono"
          style={{ maxWidth: 420 }}
          placeholder={t("e.g. status = 'active' AND id > 100")}
          value={where}
          onChange={(e) => setWhere(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              setOffset(0)
              setAppliedWhere(where.trim())
            }
          }}
        />
        <button className="icon-btn" onClick={() => load()} title={t('Refresh')}>
          <RefreshCw size={15} className={loading ? 'spin' : ''} />
        </button>
        <div className="toolbar-sep" />
        {editable ? (
          <>
            <button className="icon-btn" onClick={addRow} title={t('Add row')}>
              <Plus size={16} />
            </button>
            <button className="icon-btn" onClick={deleteSelected} disabled={!selRows.length} title={t('Delete selected rows')}>
              <Trash2 size={15} />
            </button>
            {pending > 0 && (
              <>
                <button className="btn small primary" onClick={save}>
                  <Save size={13} /> {t('Save ({n})', { n: pending })}
                </button>
                <button
                  className="btn small"
                  onClick={() => {
                    setEdits(new Map())
                    setNewRows([])
                    setDeleted(new Set())
                  }}
                >
                  <Undo2 size={13} /> {t('Discard')}
                </button>
              </>
            )}
          </>
        ) : (
          info && (
            <span className="badge" title={readOnly ? t('Connection is read-only') : t('Editing requires a primary key or unique index')}>
              <KeyRound size={11} /> {t('read-only')}
            </span>
          )
        )}
        <div className="grow" />
        <button className="icon-btn" title={t('Generate SQL')} onClick={() => info && setDialog({ type: 'generate', connectionId: tab.connectionId, dialect, table: info, columns: data?.columns, rows: selectedRowObjects(selRows) })}>
          <Wand2 size={15} />
        </button>
        <button className="icon-btn" title={t('Import data into this table')} onClick={() => setDialog({ type: 'import', connectionId: tab.connectionId, schema: tab.schema, table: tab.name })}>
          <Upload size={15} />
        </button>
        <button
          className="icon-btn"
          title={t('Export table')}
          onClick={() => setDialog({ type: 'export', dialect, defaultName: tab.name, source: { kind: 'table', connectionId: tab.connectionId, table: { schema: tab.schema, name: tab.name } } })}
        >
          <Download size={15} />
        </button>
        <button
          className="icon-btn"
          title={t('Open SELECT in SQL editor')}
          onClick={() => openSqlTab({ connectionId: tab.connectionId, schema: tab.schema, sql: `SELECT * FROM ${tab.schema && dialect !== 'sqlite' ? tab.schema + '.' : ''}${tab.name}${appliedWhere ? ` WHERE ${appliedWhere}` : ''};` })}
        >
          <FileCode2 size={15} />
        </button>
        <div className="toolbar-sep" />
        <select className="select sm" value={pageSize} onChange={(e) => (setOffset(0), setPageSize(Number(e.target.value)))} title={t('Rows per page')}>
          {[100, 200, 500, 1000, 5000].map((n) => (
            <option key={n} value={n}>
              {n}
            </option>
          ))}
        </select>
        <button className="icon-btn" disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - pageSize))} title={t('Previous page')}>
          <ChevronLeft size={16} />
        </button>
        <span className="small muted" style={{ minWidth: 90, textAlign: 'center' }}>
          {data ? `${formatCount(offset + 1)}–${formatCount(offset + data.rows.length)}` : '–'}
        </span>
        <button className="icon-btn" disabled={!data?.truncated} onClick={() => setOffset(offset + pageSize)} title={t('Next page')}>
          <ChevronRight size={16} />
        </button>
      </div>
      {error ? (
        <div className="result-message">
          <div className="error-box">{error}</div>
        </div>
      ) : data ? (
        <ResultGrid
          columns={data.columns}
          rows={rows}
          editable={editable}
          pkColumns={info?.primaryKey}
          edits={edits}
          rowState={rowState}
          onEditCell={onEditCell}
          sort={sort}
          onSort={(c) => {
            setOffset(0)
            setSort((s) => (s && s.col === c ? (s.desc ? null : { col: c, desc: true }) : { col: c, desc: false }))
          }}
          onSelection={(s) => setSelRows(s.rows)}
          extraMenu={(sel) => [
            ...(editable
              ? [
                  { label: t('Set NULL'), disabled: !sel.rows.length, onClick: () => sel.rows.forEach((r) => sel.cols.forEach((c) => onEditCell(r, c, null))) },
                  { label: t('Add row'), icon: <Plus size={14} />, onClick: addRow },
                  { label: t('Delete row(s)'), icon: <Trash2 size={14} />, danger: true, disabled: !sel.rows.length, onClick: deleteSelected },
                  { separator: true }
                ]
              : []),
            {
              label: t('Generate SQL for selected rows…'),
              icon: <Wand2 size={14} />,
              disabled: !sel.rows.length || !info,
              onClick: () => info && setDialog({ type: 'generate', connectionId: tab.connectionId, dialect, table: info, columns: data.columns, rows: selectedRowObjects(sel.rows) })
            }
          ]}
        />
      ) : (
        <div className="grid-empty">{t('Loading…')}</div>
      )}
      <div className="statusbar" style={{ borderTop: '1px solid var(--border)', height: 24 }}>
        {data && (
          <span>
            {formatCount(data.rows.length)} {t('rows')} · {formatDuration(data.durationMs)}
          </span>
        )}
        {pending > 0 && <span className="badge warn">{t('{n} unsaved change(s)', { n: pending })}</span>}
      </div>
    </>
  )
}

function StructureView({ info }: { info: TableInfo }) {
  return (
    <div className="structure">
      {info.comment && <div className="callout">{info.comment}</div>}
      <div>
        <h3>{t('Columns')}</h3>
        <table className="table-simple">
          <thead>
            <tr>
              <th style={{ width: 28 }}></th>
              <th>{t('Name')}</th>
              <th>{t('Type')}</th>
              <th>{t('Nullable')}</th>
              <th>{t('Default')}</th>
              <th>{t('Comment')}</th>
            </tr>
          </thead>
          <tbody>
            {info.columns.map((c) => (
              <tr key={c.name}>
                <td>{c.isPrimaryKey && <KeyRound size={13} color="var(--warn)" />}</td>
                <td className="mono">{c.name}</td>
                <td className="mono muted">
                  {c.dataType}
                  {c.autoIncrement && <span className="badge accent" style={{ marginLeft: 6 }}>auto</span>}
                </td>
                <td>{c.nullable ? t('yes') : <b>NOT NULL</b>}</td>
                <td className="mono small">{c.defaultValue ?? ''}</td>
                <td className="small muted">{c.comment ?? ''}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {info.indexes.length > 0 && (
        <div>
          <h3>{t('Indexes')}</h3>
          <table className="table-simple">
            <thead>
              <tr>
                <th>{t('Name')}</th>
                <th>{t('Columns')}</th>
                <th>{t('Unique')}</th>
              </tr>
            </thead>
            <tbody>
              {info.indexes.map((i) => (
                <tr key={i.name}>
                  <td className="mono">
                    {i.name} {i.primary && <span className="badge warn">PK</span>}
                  </td>
                  <td className="mono">{i.columns.join(', ')}</td>
                  <td>{i.unique ? t('yes') : ''}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {info.foreignKeys.length > 0 && (
        <div>
          <h3>{t('Foreign keys')}</h3>
          <table className="table-simple">
            <thead>
              <tr>
                <th>{t('Name')}</th>
                <th>{t('Columns')}</th>
                <th>{t('References')}</th>
                <th>ON DELETE</th>
              </tr>
            </thead>
            <tbody>
              {info.foreignKeys.map((f) => (
                <tr key={f.name}>
                  <td className="mono">{f.name}</td>
                  <td className="mono">{f.columns.join(', ')}</td>
                  <td className="mono">
                    {f.refSchema ? f.refSchema + '.' : ''}
                    {f.refTable} ({f.refColumns.join(', ')})
                  </td>
                  <td className="small">{f.onDelete ?? ''}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}

function DdlView({ tab, dialect }: { tab: TableTabT; dialect: Dialect }) {
  const [ddl, setDdl] = useState('')
  const [error, setError] = useState<string | null>(null)
  const { openSqlTab, toast } = useStore.getState()
  useEffect(() => {
    api
      .getDdl(tab.connectionId, tab.schema, tab.name, tab.objectKind)
      .then((d) => {
        setDdl(d)
        setError(null)
      })
      .catch((e) => setError(errorMessage(e)))
  }, [tab.connectionId, tab.schema, tab.name, tab.objectKind])
  if (error)
    return (
      <div className="result-message">
        <div className="error-box">{error}</div>
      </div>
    )
  return (
    <>
      <div className="toolbar">
        <button className="btn small" onClick={() => copyText(ddl).then(() => toast(t('Copied to clipboard'), 'success'))}>
          <Copy size={13} /> {t('Copy')}
        </button>
        <button className="btn small" onClick={() => openSqlTab({ connectionId: tab.connectionId, sql: ddl, title: `${tab.name}.sql` })}>
          <FileCode2 size={13} /> {t('Open in editor')}
        </button>
      </div>
      <Editor value={ddl} readOnly dialect={dialect} />
    </>
  )
}
