// SQL generator for tables and result sets.
import { useEffect, useMemo, useState } from 'react'
import { ClipboardCopy, FileCode2, Wand2 } from 'lucide-react'
import type { CellValue, ColumnMeta, Dialect, TableInfo } from '@shared/types'
import {
  generateCount,
  generateCreateTable,
  generateDeleteTemplate,
  generateDeletes,
  generateDrop,
  generateInsertTemplate,
  generateInserts,
  generateInsertsForTable,
  generateSelect,
  generateUpdateTemplate,
  generateUpdates,
  generateUpserts,
  inferColumns
} from '@shared/sqlgen'
import { quoteIdent } from '@shared/sql'
import { api } from '@/lib/api'
import { copyText } from '@/lib/format'
import { t } from '@/lib/i18n'
import { useStore } from '@/lib/store'
import { Editor } from '../Editor'
import { Modal } from '../ui'

type Kind = 'select' | 'count' | 'insert-template' | 'insert-rows' | 'update-template' | 'update-rows' | 'delete-template' | 'delete-rows' | 'upsert' | 'create' | 'create-native' | 'drop'

const DIALECTS: { d: Dialect; label: string }[] = [
  { d: 'postgres', label: 'PostgreSQL' },
  { d: 'mysql', label: 'MySQL / MariaDB' },
  { d: 'sqlite', label: 'SQLite' },
  { d: 'mssql', label: 'SQL Server' },
  { d: 'oracle', label: 'Oracle' }
]

export function GenerateDialog(props: { connectionId: string; dialect: Dialect; table?: TableInfo; columns?: ColumnMeta[]; rows?: CellValue[][] }) {
  const { setDialog, openSqlTab, toast } = useStore.getState()
  const hasRows = !!props.rows?.length
  const [kind, setKind] = useState<Kind>(props.table ? (hasRows ? 'insert-rows' : 'select') : 'insert-rows')
  const [target, setTarget] = useState<Dialect>(props.dialect)
  const [batch, setBatch] = useState(1)
  const [tableName, setTableName] = useState(props.table?.name ?? 'new_table')
  const [native, setNative] = useState<string>('')

  // Table metadata: real table or inferred from the result set
  const table: TableInfo = useMemo(() => {
    if (props.table) return props.table
    return {
      schema: '',
      name: tableName,
      kind: 'table',
      columns: inferColumns(props.columns ?? [], props.rows ?? []),
      primaryKey: [],
      indexes: [],
      foreignKeys: []
    }
  }, [props.table, props.columns, props.rows, tableName])

  const rowObjects = useMemo(() => {
    const cols = props.columns ?? table.columns.map((c) => ({ name: c.name }))
    return (props.rows ?? []).map((r) => Object.fromEntries(cols.map((c, i) => [c.name, r[i] ?? null])))
  }, [props.rows, props.columns, table])

  useEffect(() => {
    if (kind !== 'create-native' || !props.table) return
    api
      .getDdl(props.connectionId, props.table.schema, props.table.name, props.table.kind)
      .then(setNative)
      .catch((e) => setNative(`-- ${String(e)}`))
  }, [kind, props.connectionId, props.table])

  const sql = useMemo(() => {
    const d = props.dialect
    const tt = props.table ? table : { ...table, name: tableName }
    switch (kind) {
      case 'select':
        return generateSelect(tt, d)
      case 'count':
        return generateCount(tt, d)
      case 'insert-template':
        return generateInsertTemplate(tt, d)
      case 'insert-rows':
        if (!props.table && props.columns) {
          return generateInserts(quoteIdent(tableName, target), props.columns.map((c) => c.name), props.rows ?? [], target, { batchSize: batch, columnTypes: props.columns.map((c) => c.type) })
        }
        return generateInsertsForTable(tt, rowObjects, d, batch)
      case 'update-template':
        return generateUpdateTemplate(tt, d)
      case 'update-rows':
        return generateUpdates(tt, rowObjects, d)
      case 'delete-template':
        return generateDeleteTemplate(tt, d)
      case 'delete-rows':
        return generateDeletes(tt, rowObjects, d)
      case 'upsert':
        return generateUpserts(tt, rowObjects, d)
      case 'create':
        return generateCreateTable(tt, props.table ? d : 'postgres', { targetDialect: target, targetName: tableName, targetSchema: props.table && target === d ? tt.schema : '' })
      case 'create-native':
        return native
      case 'drop':
        return generateDrop(tt, d)
    }
  }, [kind, table, tableName, props, rowObjects, batch, target, native])

  const kinds: { k: Kind; label: string; needsRows?: boolean; needsTable?: boolean }[] = [
    { k: 'select', label: 'SELECT', needsTable: true },
    { k: 'count', label: 'SELECT COUNT(*)', needsTable: true },
    { k: 'insert-template', label: t('INSERT (template)'), needsTable: true },
    { k: 'insert-rows', label: t('INSERT (selected rows)'), needsRows: true },
    { k: 'update-template', label: t('UPDATE (template)'), needsTable: true },
    { k: 'update-rows', label: t('UPDATE (selected rows)'), needsRows: true, needsTable: true },
    { k: 'delete-template', label: t('DELETE (template)'), needsTable: true },
    { k: 'delete-rows', label: t('DELETE (selected rows)'), needsRows: true, needsTable: true },
    { k: 'upsert', label: t('UPSERT / MERGE (selected rows)'), needsRows: true, needsTable: true },
    { k: 'create', label: t('CREATE TABLE (generated)') },
    { k: 'create-native', label: t('DDL from database'), needsTable: true },
    { k: 'drop', label: 'DROP', needsTable: true }
  ]
  const available = kinds.filter((x) => (!x.needsRows || hasRows) && (!x.needsTable || !!props.table))
  const showTarget = kind === 'create' || (kind === 'insert-rows' && !props.table)
  const showName = !props.table || kind === 'create'

  return (
    <Modal
      title={t('Generate SQL')}
      icon={<Wand2 size={18} />}
      size="wide"
      onClose={() => setDialog(null)}
      footer={
        <>
          <span className="small muted grow">
            {props.table ? `${props.table.schema}.${props.table.name}` : t('Result set')}
            {hasRows && ` · ${t('{n} row(s) selected', { n: props.rows!.length })}`}
          </span>
          <button className="btn" onClick={() => copyText(sql).then(() => toast(t('Copied to clipboard'), 'success'))}>
            <ClipboardCopy size={14} /> {t('Copy')}
          </button>
          <button
            className="btn primary"
            onClick={() => {
              openSqlTab({ connectionId: props.connectionId || null, sql })
              setDialog(null)
            }}
          >
            <FileCode2 size={14} /> {t('Open in editor')}
          </button>
        </>
      }
    >
      <div className="row" style={{ alignItems: 'stretch', gap: 14, minHeight: 380 }}>
        <div className="col" style={{ width: 220, flex: 'none', gap: 2 }}>
          {available.map((x) => (
            <div key={x.k} className={`tree-row ${kind === x.k ? 'selected' : ''}`} style={{ borderRadius: 6, paddingLeft: 10 }} onClick={() => setKind(x.k)}>
              <span className="tlabel">{x.label}</span>
            </div>
          ))}
        </div>
        <div className="col grow" style={{ minWidth: 0 }}>
          {(showTarget || showName || kind === 'insert-rows') && (
            <div className="row" style={{ flexWrap: 'wrap' }}>
              {showName && (
                <div className="field" style={{ width: 200 }}>
                  <label>{t('Table name')}</label>
                  <input className="input sm mono" value={tableName} onChange={(e) => setTableName(e.target.value)} />
                </div>
              )}
              {showTarget && (
                <div className="field" style={{ width: 200 }}>
                  <label>{t('Target dialect')}</label>
                  <select className="select sm" value={target} onChange={(e) => setTarget(e.target.value as Dialect)}>
                    {DIALECTS.map((d) => (
                      <option key={d.d} value={d.d}>
                        {d.label}
                      </option>
                    ))}
                  </select>
                </div>
              )}
              {kind === 'insert-rows' && (
                <div className="field" style={{ width: 160 }}>
                  <label>{t('Rows per INSERT')}</label>
                  <input className="input sm" type="number" min={1} max={1000} value={batch} onChange={(e) => setBatch(Number(e.target.value) || 1)} />
                </div>
              )}
            </div>
          )}
          <div style={{ flex: 1, minHeight: 300, border: '1px solid var(--border)', borderRadius: 8, overflow: 'hidden', display: 'flex' }}>
            <Editor value={sql} readOnly dialect={showTarget ? target : props.dialect} />
          </div>
        </div>
      </div>
    </Modal>
  )
}
